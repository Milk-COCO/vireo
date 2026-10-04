#!/usr/bin/env pwsh
# 16 KB page size 合规检查（Windows / PowerShell）
#
# 为什么需要这个脚本：Google 官方的 `check_elf_alignment.sh` **只检查 PT_LOAD 的
# p_align**，完全不看 RELRO。而 RELRO 不对齐会在 16 KB 设备上**运行时 segfault**
# —— 官方工具因此会给假绿灯。
#
# 两条判据（`developer.android.com/guide/practices/page-sizes`）：
#   1. 每个 PT_LOAD 的 p_align >= 0x4000
#   2. PT_GNU_RELRO 的 (VirtAddr + MemSiz) % 0x4000 == 0
#
# 用法：
#   pwsh check-16kb.ps1                      # 检查 src/main/jniLibs 下的 .so
#   pwsh check-16kb.ps1 -Apk build/...apk   # 额外用 zipalign 校验 APK
#
# 退出码：0 = 全过，1 = 有不合规项。

param(
    [string]$SoDir = 'src/main/jniLibs',
    [string]$NdkVersion = '',
    [string]$Apk = ''
)

$ErrorActionPreference = 'Stop'

# ---- 定位 llvm-readelf / zipalign ----------------------------------------
# SDK 根目录：环境变量 > local.properties 的 sdk.dir > Windows 默认位置
function Get-SdkRoot {
    foreach ($v in @($env:ANDROID_HOME, $env:ANDROID_SDK_ROOT)) {
        if ($v -and (Test-Path $v)) { return $v }
    }
    $lp = 'local.properties'
    if (Test-Path $lp) {
        foreach ($line in Get-Content $lp) {
            if ($line -match '^\s*sdk\.dir\s*=\s*(.+?)\s*$') {
                $d = $Matches[1] -replace '\\\\', '\'
                if (Test-Path $d) { return $d }
            }
        }
    }
    foreach ($d in @("$env:LOCALAPPDATA\Android\Sdk", "$env:USERPROFILE\AppData\Local\Android\Sdk")) {
        if (Test-Path $d) { return $d }
    }
    return $null
}

function Find-Tool([string[]]$names, [string]$subdir) {
    $c = Get-Command $names[0] -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    $root = Get-SdkRoot
    if (-not $root) {
        throw ("找不到 {0}，也定位不到 Android SDK。" -f $names[0]) +
              "`n       设 ANDROID_HOME，或在 local.properties 写 sdk.dir=..."
    }
    # zipalign 在 build-tools 下，不在 ndk/<ver>/ 下 —— 别让 NdkVersion 把它限死
    $base = if ($subdir -eq 'ndk' -and $NdkVersion) { Join-Path $root "ndk/$NdkVersion" } else { $root }
    foreach ($n in $names) {
        $hit = Get-ChildItem $base -Recurse -Filter $n -ErrorAction SilentlyContinue |
               Sort-Object FullName -Descending | Select-Object -First 1
        if ($hit) { return $hit.FullName }
    }
    throw ("在 {0} 下找不到 {1}" -f $base, $names[0])
}

$readelf = Find-Tool @('llvm-readelf.exe', 'llvm-readelf') 'ndk'
$zipalign = $null
try { $zipalign = Find-Tool @('zipalign.exe', 'zipalign') 'build-tools' } catch { }

Write-Host "readelf : $readelf"
if ($zipalign) { Write-Host "zipalign: $zipalign" }

$ALIGN = 0x4000
$fail = 0
$checked = 0

# ---- ELF 检查 -------------------------------------------------------------
$targets = if (Test-Path $SoDir) {
    Get-ChildItem $SoDir -Recurse -Filter '*.so' -ErrorAction SilentlyContinue
} else { @() }

if (-not $targets) { Write-Warning "没找到 .so（$SoDir）—— 先构建过再说。" }

foreach ($so in $targets) {
    # x86_64 模拟器不要求 RELRO 对齐（官方：仅「为测试目的」支持大页）
    $isX86 = $so.FullName -match 'x86_64'
    $lines = & $readelf -lW $so.FullName 2>&1

    $loads = @($lines | Where-Object { $_ -match '^\s*LOAD\s' } | ForEach-Object {
        ($_.Trim() -split '\s+')[-1]
    })
    $relro = @($lines | Where-Object { $_ -match '^\s*GNU_RELRO\s' } | Select-Object -First 1)

    $label = $so.FullName.Substring((Get-Location).Path.Length + 1)

    # 判据 1：所有 PT_LOAD 的 p_align
    $badLoad = @($loads | Where-Object {
        $v = [Convert]::ToInt32($_.Substring(2), 16); $v -lt $ALIGN
    })
    $loadOk = ($loads.Count -gt 0 -and $badLoad.Count -eq 0)

    # 判据 2：RELRO 末边界。列序：Type Offset VirtAddr PhysAddr FileSiz MemSiz Flg Align
    $relroTxt = 'n/a'
    $relroOk = $true
    if ($relro.Count -gt 0) {
        $p = $relro[0].Trim() -split '\s+'
        $vaddr = [Convert]::ToInt64($p[3], 16)
        $memsz = [Convert]::ToInt64($p[5], 16)
        $end = $vaddr + $memsz
        $relroOk = (($end % $ALIGN) -eq 0)
        $relroTxt = ('end=0x{0:x} mod=0x{1:x}{2}' -f $end, ($end % $ALIGN),
            $(if ($isX86 -and -not $relroOk) { ' (x86_64 不要求)' } else { '' }))
        if ($isX86) { $relroOk = $true }   # 见上：官方只对 arm64-v8a 要求
    }

    $checked++
    $ok = $loadOk -and $relroOk
    if (-not $ok) { $fail++ }
    $tag = if ($ok) { 'PASS' } else { 'FAIL' }
    Write-Host ("[{0}] {1}" -f $tag, $label) `
        -ForegroundColor $(if ($ok) { 'Green' } else { 'Red' })
    Write-Host ("       LOAD  : {0}  ({1} 个, 需 >= 0x4000)" -f ($loads -join ' '), $loads.Count)
    Write-Host ("       RELRO : {0}" -f $relroTxt)
}

# ---- APK 检查 -------------------------------------------------------------
if ($Apk) {
    if (-not (Test-Path $Apk)) { throw "APK 不存在: $Apk" }
    if (-not $zipalign) { Write-Warning '没找到 zipalign，跳过 APK 检查。' }
    else {
        Write-Host ''
        Write-Host "== APK: $Apk"
        & $zipalign -c -P 16 -v 4 $Apk | Select-Object -Last 3
        if ($LASTEXITCODE -ne 0) {
            Write-Host '[FAIL] zipalign 校验未通过' -ForegroundColor Red
            $fail++
        } else {
            Write-Host '[PASS] zipalign 校验通过（.so 均为 16 KB 对齐）' -ForegroundColor Green
        }
    }
}

# ---- AAB 提示 -------------------------------------------------------------
Write-Host ''
Write-Host '提示：若改发 .aab，bundletool 不会自动 zipalign，需另外确认：'
Write-Host '      bundletool dump config --bundle=app-release.aab | Select-String alignment'
Write-Host '      期望 PAGE_ALIGNMENT_16K（PAGE_ALIGNMENT_4K 会导致 Play 侧安装失败）'
Write-Host '提示：静态检查全绿 ≠ 16 KB 真机不炸，仍需 adb shell getconf PAGE_SIZE 确认。'

Write-Host ''
if ($fail -eq 0) {
    Write-Host "全部通过（检查了 $checked 个 .so）" -ForegroundColor Green
    exit 0
} else {
    Write-Host "$fail 项不合规" -ForegroundColor Red
    exit 1
}