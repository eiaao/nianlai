#Requires -Version 5.1
<#
.SYNOPSIS
  构建并打包拈来 Windows x64 便携版，不制作安装包或签名。
.DESCRIPTION
  版本以 src-tauri/Cargo.toml 为准；Tauri 配置省略 version，package.json 自动同步。
  先拒绝未提交改动，再安装依赖、按需构建，生成 ZIP、SHA-256 和构建信息。
  目标目录通过 Cargo 元数据读取，遵守 CARGO_TARGET_DIR 和 Cargo 的目录配置。
.PARAMETER Version
  可选的 SemVer 版本，例如 0.1.0-alpha.1；不传则使用 Cargo.toml 的版本。
  指定新版本会修改版本清单和 Cargo.lock，请在发布后审查并提交这些改动。
.PARAMETER SkipBuild
  跳过正式版构建，直接打包已有程序；仍同步版本并安装前端依赖。
  不核验已有程序的内嵌版本；构建信息明确记录跳过构建和程序文件时间。
.PARAMETER OutDir
  输出目录，默认是仓库根目录的 dist；相对路径始终相对于仓库根目录。
.PARAMETER AllowDirty
  允许工作区有未提交改动；构建信息会记录最终是否存在未提交改动。
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1
.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1 -SkipBuild -OutDir dist\自测
#>
[CmdletBinding()]
param(
    [string]$Version,
    [switch]$SkipBuild,
    [ValidateNotNullOrEmpty()][string]$OutDir = 'dist',
    [switch]$AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:Utf8Bom = New-Object System.Text.UTF8Encoding($true)
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
try { [Console]::OutputEncoding = $script:Utf8NoBom } catch { }

function Write-ReleaseText([string]$Path, [string]$Content) {
    $text = $Content -replace '\r?\n', "`r`n"
    [IO.File]::WriteAllText($Path, $text, $script:Utf8Bom)
}

function Set-ManifestText([string]$Path, [string]$Content) {
    if ([IO.File]::ReadAllText($Path) -cne $Content) {
        [IO.File]::WriteAllText($Path, $Content, $script:Utf8NoBom)
    }
}

function Assert-Command([string]$Name) {
    if ($LASTEXITCODE -ne 0) {
        throw ("{0}失败，退出码 {1}；已停止发布。" -f $Name, $LASTEXITCODE)
    }
}

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..')).TrimEnd('\', '/')
$staging = $null
Push-Location -LiteralPath $repoRoot
try {
    $gitRoot = & git rev-parse --show-toplevel
    Assert-Command '读取 Git 工作区'
    if ([IO.Path]::GetFullPath($gitRoot).TrimEnd('\', '/') -ne $repoRoot) {
        throw '脚本必须位于当前 Git 工作区的 tools 目录中。'
    }
    $gitStatus = @(& git status --porcelain=v1 --untracked-files=all)
    Assert-Command '检查 Git 状态'
    if ($gitStatus.Count -gt 0 -and -not $AllowDirty) {
        throw '工作区有未提交改动，请先提交；确需带改动打包时使用 -AllowDirty。'
    }
    $gitCommit = & git rev-parse HEAD
    Assert-Command '读取 Git 提交'

    $cargoPath = Join-Path $repoRoot 'src-tauri\Cargo.toml'
    $packagePath = Join-Path $repoRoot 'package.json'
    $cargoText = [IO.File]::ReadAllText($cargoPath)
    $packageSection = [regex]::Match($cargoText, '(?ms)^\[package\][ \t]*\r?\n(?<body>.*?)(?=^\[|\z)')
    $cargoVersion = [regex]::Match($packageSection.Groups['body'].Value, '(?m)^version[ \t]*=[ \t]*"(?<version>[^"\r\n]+)"')
    if (-not $packageSection.Success -or -not $cargoVersion.Success) {
        throw '无法读取 src-tauri/Cargo.toml 的应用版本。'
    }
    if (-not $PSBoundParameters.ContainsKey('Version')) {
        $Version = $cargoVersion.Groups['version'].Value
    }
    # 数字预发布标识不能有前导零；构建元数据允许数字和连字符。
    $number = '(0|[1-9][0-9]*)'
    $pre = '(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)'
    $semver = '\A' + $number + '\.' + $number + '\.' + $number + '(-' + $pre + '(\.' + $pre + ')*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?\z'
    if ([string]::IsNullOrEmpty($Version) -or $Version -cnotmatch $semver) {
        throw '版本号必须是有效的 SemVer，例如 0.1.0-alpha.1。'
    }
    $packageText = [IO.File]::ReadAllText($packagePath)
    $packageVersion = [regex]::Matches($packageText, '"version"\s*:\s*"(?<version>[^"\r\n]*)"')
    if ($packageVersion.Count -ne 1 -or -not ($packageText | ConvertFrom-Json).version) {
        throw 'package.json 必须包含唯一的应用 version 字段。'
    }
    $config = [IO.File]::ReadAllText((Join-Path $repoRoot 'src-tauri\tauri.conf.json')) | ConvertFrom-Json
    if ($config.PSObject.Properties.Name -contains 'version') {
        throw '请移除 tauri.conf.json 的 version 字段，让 Tauri 使用 Cargo.toml 的版本。'
    }

    Write-Host ("发布版本：{0}" -f $Version)
    $versionGroup = $cargoVersion.Groups['version']
    $versionOffset = $packageSection.Groups['body'].Index + $versionGroup.Index
    $cargoText = $cargoText.Remove($versionOffset, $versionGroup.Length).Insert($versionOffset, $Version)
    $versionGroup = $packageVersion[0].Groups['version']
    $packageText = $packageText.Remove($versionGroup.Index, $versionGroup.Length).Insert($versionGroup.Index, $Version)
    Set-ManifestText $cargoPath $cargoText
    Set-ManifestText $packagePath $packageText

    # 只更新工作区包的锁记录，不升级已锁定的外部依赖。
    Write-Host '同步 Cargo 锁文件……'
    & cargo update --workspace
    Assert-Command '同步 Cargo 锁文件'
    Write-Host '读取 Cargo 目标目录……'
    $metadataText = & cargo metadata --format-version 1 --no-deps
    Assert-Command '读取 Cargo 元数据'
    $metadata = ($metadataText -join "`n") | ConvertFrom-Json
    $exePath = Join-Path $metadata.target_directory 'release\nianlai.exe'
    $rustc = & rustc --version
    Assert-Command '读取 Rust 编译器版本'

    Write-Host '安装前端依赖（使用冻结锁文件）……'
    & $env:ComSpec /d /c 'pnpm install --frozen-lockfile'
    Assert-Command '安装前端依赖'
    if ($SkipBuild) {
        Write-Host '已跳过正式版构建；将打包已有程序，请自行确认其内嵌版本。'
    } else {
        Write-Host '开始正式版构建（不生成安装包）……'
        & $env:ComSpec /d /c 'pnpm tauri build --no-bundle'
        Assert-Command '正式版构建'
    }
    if (-not (Test-Path -LiteralPath $exePath -PathType Leaf)) {
        throw ("找不到程序：{0}；请先完成构建。" -f $exePath)
    }
    $gitStatus = @(& git status --porcelain=v1 --untracked-files=all)
    Assert-Command '记录最终 Git 状态'

    $outputPath = if ([IO.Path]::IsPathRooted($OutDir)) {
        [IO.Path]::GetFullPath($OutDir)
    } else {
        [IO.Path]::GetFullPath((Join-Path $repoRoot $OutDir))
    }
    [IO.Directory]::CreateDirectory($outputPath) | Out-Null
    $staging = Join-Path $outputPath ('.nianlai-release-' + [Guid]::NewGuid().ToString('N'))
    $payload = Join-Path $staging 'payload'
    [IO.Directory]::CreateDirectory($payload) | Out-Null
    $packagedExe = Join-Path $payload 'nianlai.exe'
    Copy-Item -LiteralPath $exePath -Destination $packagedExe
    $instructions = @"
拈来 v$Version · Windows x64 便携版

运行：将 ZIP 完整解压到可写文件夹，双击 nianlai.exe，无需安装。
运行前需要 Microsoft Edge WebView2 运行时（Windows 11 通常已自带）；若缺失，请先安装微软的 WebView2 Evergreen 运行时。
首次运行若遇到 Windows SmartScreen 提示，请在确认文件来源后选择“更多信息 → 仍要运行”。此版本未签名。

使用：默认按 Alt+V 呼出面板；若快捷键被占用则尝试 Ctrl+Shift+F12，亦可用 NIANLAI_HOTKEY 环境变量指定呼出键。
退出程序请按 Ctrl+Alt+Shift+Q。
数据目录：默认 %LOCALAPPDATA%\Nianlai\g0\data，诊断日志位于 %LOCALAPPDATA%\Nianlai\g0\panel-log.jsonl。
若设置了 NIANLAI_G0_DATA_DIR 环境变量，历史数据和日志使用该变量指定的目录。
便携版表示无需安装，历史数据仍保存在上述用户目录中。
"@
    Write-ReleaseText (Join-Path $payload '使用说明.txt') ($instructions + "`r`n")

    $zipName = "nianlai-$Version-windows-x64-portable.zip"
    $checksumName = "nianlai-$Version.sha256"
    $temporaryZip = Join-Path $staging $zipName
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::CreateFromDirectory($payload, $temporaryZip, [IO.Compression.CompressionLevel]::Optimal, $false)
    $zipHash = (Get-FileHash -LiteralPath $temporaryZip -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-ReleaseText (Join-Path $staging $checksumName) ("$zipHash *$zipName`r`n")
    $buildInfo = [ordered]@{
        version = $Version
        git_commit = $gitCommit
        git_dirty = ($gitStatus.Count -gt 0)
        build_time_utc = (Get-Item -LiteralPath $packagedExe).LastWriteTimeUtc.ToString('o')
        packaged_at_utc = [DateTime]::UtcNow.ToString('o')
        rustc_version = ($rustc -join ' ')
        skip_build = [bool]$SkipBuild
        exe_sha256 = (Get-FileHash -LiteralPath $packagedExe -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    Write-ReleaseText (Join-Path $staging 'build-info.json') (($buildInfo | ConvertTo-Json) + "`r`n")
    foreach ($name in @($zipName, $checksumName, 'build-info.json')) {
        Move-Item -LiteralPath (Join-Path $staging $name) -Destination (Join-Path $outputPath $name) -Force
    }
    Write-Host ("发布完成：{0}" -f (Join-Path $outputPath $zipName))
    Write-Host ("校验文件：{0}" -f (Join-Path $outputPath $checksumName))
    Write-Host ("构建信息：{0}" -f (Join-Path $outputPath 'build-info.json'))
} catch {
    Write-Host ("发布失败：{0}" -f $_.Exception.Message) -ForegroundColor Red
    exit 1
} finally {
    if ($staging -and (Test-Path -LiteralPath $staging)) {
        # 删除前确认解析后的绝对路径仍位于输出目录，只清理本次创建的暂存目录。
        $resolvedStaging = (Resolve-Path -LiteralPath $staging).Path
        $outputPrefix = [IO.Path]::GetFullPath($outputPath).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
        if ($resolvedStaging.StartsWith($outputPrefix, [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolvedStaging) -like '.nianlai-release-*') {
            Remove-Item -LiteralPath $resolvedStaging -Recurse -Force
        }
    }
    Pop-Location
}
