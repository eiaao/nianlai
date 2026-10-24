<#
.SYNOPSIS
  整夜采样拈来样机及其 WebView2 子进程的内存，验证挂起与周期性重挂起的效果（G0）。

.DESCRIPTION
  从被测进程出发沿父子关系收集整棵进程树，按固定计划时刻采样专用工作集、工作集、私有提交和累计缺页，
  分别写入 processes.csv 和 totals.csv；结束时把样机日志中本次运行新增的部分切出来，并生成 summary.txt 摘要。
  可由脚本启动样机（-Exe，配合 -ResuspendMinutes、-AutoCycle），也可附着到已运行的进程（-ProcessId 或 -Name）。
  采样时刻按“开始时刻 + k × 间隔”计算，不随采样耗时而累积漂移。
  默认运行期间阻止系统自动睡眠（显示器不受影响），加 -AllowSleep 可关闭该行为。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools/measure-memory-overnight.ps1 -Exe target\release\nianlai.exe -ResuspendMinutes 30
  每 30 分钟在隐藏状态下重挂起一次 WebView2，跑默认的 8 小时。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools/measure-memory-overnight.ps1 -Exe target\release\nianlai.exe -ResuspendMinutes 0 -Label baseline
  不设重挂起环境变量（基线对照），输出目录用 baseline 标记。

.EXAMPLE
  pwsh -NoProfile -File tools/measure-memory-overnight.ps1 -ProcessId 4242 -DurationHours 1
  附着到已运行的样机进程，只采样 1 小时，结束时不动它。
#>
param(
  [string]$Exe = "",
  [int]$ProcessId = 0,
  [string]$Name = "nianlai",
  [double]$ResuspendMinutes = 0,
  [string]$AutoCycle = "5:3,5:3,5:3",
  [double]$DurationHours = 8,
  [int]$IntervalSeconds = 60,
  [string]$Label = "",
  [string]$OutDir = "",
  [switch]$AllowSleep
)

try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

# 样机按小数点解析分钟数，不能随系统区域设置写成逗号。
$minutesText = $ResuspendMinutes.ToString([System.Globalization.CultureInfo]::InvariantCulture)
if ([string]::IsNullOrWhiteSpace($Label)) {
  if ($ResuspendMinutes -gt 0) { $Label = "resuspend$minutesText" } else { $Label = "baseline" }
}
if ([string]::IsNullOrWhiteSpace($OutDir)) {
  $OutDir = Join-Path $repoRoot ("target\g0-overnight\{0:yyyyMMdd-HHmmss}-{1}" -f (Get-Date), $Label)
}

function Format-Number([double]$Value, [string]$Pattern) {
  $Value.ToString($Pattern, [System.Globalization.CultureInfo]::InvariantCulture)
}

# WebView2 子进程的类型写在命令行的 --type 和 --utility-sub-type 里；没有 --type 的是浏览器主进程。只用 ASCII。
function Get-ProcessKind([string]$ProcessName, [string]$CommandLine) {
  if ($ProcessName -ne "msedgewebview2") { return "app" }
  if ($CommandLine -match "--utility-sub-type=([\w.]+)") { return ($Matches[1] -replace "\.mojom\.", ":") }
  if ($CommandLine -match "--type=([\w-]+)") { return $Matches[1] }
  return "browser"
}

# 表头只在文件新建时写一次；正文用不带 BOM 的 UTF-8 追加。
function Add-CsvLines([string]$Path, [string]$Header, $Lines) {
  if (-not (Test-Path $Path)) { [System.IO.File]::AppendAllText($Path, $Header + "`r`n", $script:Utf8NoBom) }
  if ($Lines.Count -gt 0) { [System.IO.File]::AppendAllText($Path, (($Lines -join "`r`n") + "`r`n"), $script:Utf8NoBom) }
}

function Get-ValueAtMinutes([double]$Minutes) {
  $best = $null
  $bestDiff = [double]::MaxValue
  foreach ($row in $script:TotalsData) {
    $diff = [math]::Abs($row.elapsed_min - $Minutes)
    if ($diff -lt $bestDiff) { $bestDiff = $diff; $best = $row }
  }
  # 没跑到这个时间点就不拿最后一次冒充。
  if ($null -eq $best -or $bestDiff -gt [math]::Max(2.0, 2.0 * $IntervalSeconds / 60.0)) { return $null }
  return (Format-Number ([math]::Round($best.private_ws_mb, 1)) "0.0")
}

# 最小二乘法算斜率（MB/小时），x 轴是小时。
function Get-SlopeMbPerHour([double]$FromMinutes) {
  $points = @($script:TotalsData | Where-Object { $_.elapsed_min -ge $FromMinutes })
  if ($points.Count -lt 2) { return $null }
  $n = $points.Count
  $sx = 0.0; $sy = 0.0; $sxx = 0.0; $sxy = 0.0
  foreach ($p in $points) {
    $x = $p.elapsed_min / 60.0
    $y = $p.private_ws_mb
    $sx += $x; $sy += $y; $sxx += $x * $x; $sxy += $x * $y
  }
  $den = $n * $sxx - $sx * $sx
  if ($den -le 0) { return $null }
  return (Format-Number ([math]::Round(($n * $sxy - $sx * $sy) / $den, 2)) "0.00")
}

function Restore-ScriptEnv {
  foreach ($key in @($script:EnvBackup.Keys)) {
    $old = $script:EnvBackup[$key]
    if ($null -eq $old) {
      Remove-Item -Path ("Env:" + $key) -ErrorAction SilentlyContinue
    } else {
      Set-Item -Path ("Env:" + $key) -Value $old
    }
  }
}

# ---------- 启动前的检查与准备 ----------
$script:StartedByScript = $false
$script:RootPid = 0
$script:ChildEnvSet = [ordered]@{}
$script:ChildEnvRemoved = @()
$script:EnvBackup = @{}
$script:ExecutionStateSet = $false
$script:TotalsData = New-Object System.Collections.Generic.List[object]
$script:LastTreePids = @()
$script:LastSampleAt = [datetime]::MinValue
$script:LeftoverKilled = @()
$script:GapSeen = $false
$script:EndedReason = "被中断（Ctrl+C 或异常）"

$launchMode = -not [string]::IsNullOrWhiteSpace($Exe)
if ($launchMode) {
  if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
    Write-Error "找不到要启动的程序：$Exe"
    exit 1
  }
  $running = @(Get-Process -Name "nianlai" -ErrorAction SilentlyContinue)
  if ($running.Count -gt 0) {
    $ids = ($running | ForEach-Object { $_.Id }) -join ", "
    Write-Error "已有 nianlai.exe 在运行（PID $ids）。两个样机会共用同一个 WebView2 用户数据目录和退出快捷键，数据会互相污染；请先退出它再运行本脚本。"
    exit 1
  }
}

# 记下样机日志当前的长度，结束时用来切出本次新增的部分。
$panelLog = Join-Path $env:LOCALAPPDATA "Nianlai\g0\panel-log.jsonl"
$panelLogStart = 0
if (Test-Path $panelLog) { $panelLogStart = (Get-Item $panelLog).Length }

$script:StartedAt = Get-Date
$exePath = ""
if ($launchMode) {
  $exePath = (Resolve-Path $Exe).Path
  # 子进程环境：NIANLAI_MEMORY_TIER 一律删除（确保样机在挂起这一档）；重挂起间隔 >0 设置、==0 删除（基线）。
  $toSet = [ordered]@{}
  $toRemove = @("NIANLAI_MEMORY_TIER")
  if ($ResuspendMinutes -gt 0) { $toSet["NIANLAI_G0_RESUSPEND_MIN"] = $minutesText }
  else { $toRemove += "NIANLAI_G0_RESUSPEND_MIN" }
  if (-not [string]::IsNullOrEmpty($AutoCycle)) { $toSet["NIANLAI_G0_AUTOCYCLE"] = $AutoCycle }
  $script:ChildEnvSet = $toSet
  $script:ChildEnvRemoved = $toRemove
  foreach ($name in $toRemove) { $script:EnvBackup[$name] = [Environment]::GetEnvironmentVariable($name, "Process") }
  foreach ($name in $toSet.Keys) { $script:EnvBackup[$name] = [Environment]::GetEnvironmentVariable($name, "Process") }
  try {
    foreach ($name in $toRemove) { Remove-Item -Path ("Env:" + $name) -ErrorAction SilentlyContinue }
    foreach ($name in $toSet.Keys) { Set-Item -Path ("Env:" + $name) -Value $toSet[$name] }
    $workDir = Split-Path -Parent $exePath
    if ([string]::IsNullOrEmpty($workDir)) {
      $app = Start-Process -FilePath $exePath -PassThru
    } else {
      $app = Start-Process -FilePath $exePath -WorkingDirectory $workDir -PassThru
    }
  } finally {
    # 启动完立刻恢复本进程的环境变量。
    Restore-ScriptEnv
  }
  # 启动失败时 PID 会是空或 0，不能拿它去收集进程树，更不能在结束时去停它。
  if ($null -eq $app -or $app.Id -le 0) {
    Write-Error "样机没有启动成功：$exePath"
    exit 1
  }
  $script:RootPid = $app.Id
  $script:StartedByScript = $true
  Write-Host "已启动样机：$exePath（PID $($script:RootPid)）"
} else {
  if ($ProcessId -gt 0) {
    if ($null -eq (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) {
      Write-Error "没有找到 PID 为 $ProcessId 的进程。"
      exit 1
    }
    $script:RootPid = $ProcessId
  } else {
    $found = @(Get-Process -Name $Name -ErrorAction SilentlyContinue)
    if ($found.Count -eq 0) { Write-Error "没有找到进程 $Name"; exit 1 }
    if ($found.Count -gt 1) {
      Write-Error "按名字 $Name 找到 $($found.Count) 个进程，请改用 -ProcessId 指定其中一个。"
      exit 1
    }
    $script:RootPid = $found[0].Id
  }
  Write-Host "附着到已运行的进程：$Name（PID $($script:RootPid)）"
}

# 默认阻止系统自动睡眠（ES_CONTINUOUS | ES_SYSTEM_REQUIRED），显示器照常可以关。
Add-Type -Namespace Nianlai -Name Power -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint esFlags);'
$esContinuous = [System.Convert]::ToUInt32("80000000", 16)
$esSystemRequired = [System.Convert]::ToUInt32("1", 16)
if (-not $AllowSleep) {
  [void][Nianlai.Power]::SetThreadExecutionState($esContinuous -bor $esSystemRequired)
  $script:ExecutionStateSet = $true
}

# ---------- run.json ----------
New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
$osInfo = Get-CimInstance Win32_OperatingSystem
$webview2Version = $null
foreach ($key in @("HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}", "HKCU:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}")) {
  if ($null -eq $webview2Version -and (Test-Path $key)) {
    $pv = (Get-ItemProperty -Path $key -ErrorAction SilentlyContinue).pv
    if ($pv) { $webview2Version = [string]$pv }
  }
}
$runInfo = [ordered]@{
  label = $Label
  exe = $Exe
  process_id = $ProcessId
  name = $Name
  resuspend_minutes = $ResuspendMinutes
  autocycle = $AutoCycle
  duration_hours = $DurationHours
  interval_seconds = $IntervalSeconds
  allow_sleep = [bool]$AllowSleep
  started_at = $script:StartedAt.ToString("yyyy-MM-ddTHH:mm:ss")
  hostname = $env:COMPUTERNAME
  os = [ordered]@{
    caption = [string]$osInfo.Caption
    version = [string]$osInfo.Version
    build = [string]$osInfo.BuildNumber
  }
  physical_memory_mb = [math]::Round($osInfo.TotalVisibleMemorySize / 1024, 0)
  available_memory_mb = [math]::Round($osInfo.FreePhysicalMemory / 1024, 0)
  webview2_runtime_version = $webview2Version
  webview2_additional_browser_arguments = $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS
  pid = $script:RootPid
  child_env_set = $script:ChildEnvSet
  child_env_removed = $script:ChildEnvRemoved
}
if ($launchMode) {
  $runInfo["exe_path"] = $exePath
  $runInfo["exe_sha256"] = (Get-FileHash -Path $exePath -Algorithm SHA256).Hash
  $runInfo["exe_last_write_utc"] = (Get-Item $exePath).LastWriteTimeUtc.ToString("yyyy-MM-ddTHH:mm:ssZ")
}
[System.IO.File]::WriteAllText((Join-Path $OutDir "run.json"), ($runInfo | ConvertTo-Json -Depth 4), $script:Utf8NoBom)

# ---------- 采样循环 ----------
$totalsPath = Join-Path $OutDir "totals.csv"
$processesPath = Join-Path $OutDir "processes.csv"
$totalsHeader = "time,elapsed_min,sample,processes,private_ws_mb,webview2_private_ws_mb,app_private_ws_mb,private_bytes_mb,available_mb,gap_s"
$processesHeader = "time,elapsed_min,sample,pid,parent_pid,kind,private_ws_mb,working_set_mb,private_bytes_mb,page_faults,cpu_s"
$deadline = $script:StartedAt.AddHours($DurationHours)
$previousAt = $script:StartedAt
$sampleIndex = 0

Write-Host "输出目录：$OutDir"
Write-Host "开始测量（$Label）：计划 $DurationHours 小时，每 $IntervalSeconds 秒采样一次。"

try {
  while ($true) {
    # 第 k 次采样安排在开始时刻 + k × 间隔，不累积漂移。
    $nextAt = $script:StartedAt.AddSeconds([long]$IntervalSeconds * ($sampleIndex + 1))
    $waitMs = ($nextAt - (Get-Date)).TotalMilliseconds
    if ($waitMs -gt 0) { Start-Sleep -Milliseconds $waitMs }
    if ((Get-Date) -ge $deadline) { break }
    $now = Get-Date
    $sampleIndex++
    $timeText = $now.ToString("yyyy-MM-ddTHH:mm:ss")
    $elapsedMin = ($now - $script:StartedAt).TotalMinutes
    $elapsedText = (Format-Number ([math]::Round($elapsedMin, 2)) "0.##")

    $gapText = ""
    $gap = ($now - $previousAt).TotalSeconds
    if ($gap -gt 2 * $IntervalSeconds) {
      $gapText = (Format-Number ([math]::Round($gap, 1)) "0.0")
      $script:GapSeen = $true
      Write-Host "[警告] 距上次采样 $gapText 秒，超过计划间隔的两倍，可能系统睡眠过或机器卡顿。"
    }
    $previousAt = $now

    $allProcesses = @(Get-CimInstance Win32_Process)
    $byId = @{}
    $childrenOf = @{}
    foreach ($p in $allProcesses) {
      $procIdNum = [uint32]$p.ProcessId
      $byId[$procIdNum] = $p
      $parentNum = [uint32]$p.ParentProcessId
      if (-not $childrenOf.ContainsKey($parentNum)) { $childrenOf[$parentNum] = New-Object System.Collections.Generic.List[object] }
      $childrenOf[$parentNum].Add($p)
    }
    if (-not $byId.ContainsKey([uint32]$script:RootPid)) {
      Write-Host "根进程 PID $($script:RootPid) 已退出，结束采样。"
      $script:EndedReason = "根进程退出"
      break
    }
    $tree = New-Object System.Collections.Generic.List[object]
    $seen = @{ [uint32]$script:RootPid = $true }
    $queue = New-Object System.Collections.Generic.Queue[uint32]
    $queue.Enqueue([uint32]$script:RootPid)
    while ($queue.Count -gt 0) {
      $cur = $queue.Dequeue()
      $tree.Add($byId[$cur])
      if ($childrenOf.ContainsKey($cur)) {
        foreach ($child in $childrenOf[$cur]) {
          $childId = [uint32]$child.ProcessId
          if (-not $seen.ContainsKey($childId)) { $seen[$childId] = $true; $queue.Enqueue($childId) }
        }
      }
    }
    $script:LastTreePids = @($tree | ForEach-Object { [uint32]$_.ProcessId })
    $script:LastSampleAt = $now

    # 原始值：WorkingSetPrivate 是专用工作集（任务管理器“内存”列的口径），PageFaultsPersec 是累计缺页数。
    $perf = @{}
    foreach ($instance in (Get-CimInstance Win32_PerfRawData_PerfProc_Process)) {
      $perf[[uint32]$instance.IDProcess] = $instance
    }
    $freeKb = (Get-CimInstance Win32_OperatingSystem -Property FreePhysicalMemory).FreePhysicalMemory
    $availableMb = [double]$freeKb / 1024.0

    $lines = New-Object System.Collections.Generic.List[string]
    $sumPrivateWs = 0.0; $sumWebviewWs = 0.0; $sumAppWs = 0.0; $sumPrivateBytes = 0.0
    foreach ($p in $tree) {
      $rowPid = [uint32]$p.ProcessId
      $privateWs = 0.0; $workingSet = 0.0; $privateBytes = 0.0; $pageFaults = 0
      $perfRow = $perf[$rowPid]
      if ($null -ne $perfRow) {
        $privateWs = [double]$perfRow.WorkingSetPrivate / 1MB
        $workingSet = [double]$perfRow.WorkingSet / 1MB
        $privateBytes = [double]$perfRow.PrivateBytes / 1MB
        $pageFaults = [uint64]$perfRow.PageFaultsPersec
      }
      $cpuSeconds = ([double]$p.KernelModeTime + [double]$p.UserModeTime) / 10000000.0
      # Win32_Process 的 Name 带 .exe 后缀，先去掉再和进程名比较。
      $procName = ([string]$p.Name) -replace '\.exe$', ''
      $kind = Get-ProcessKind -ProcessName $procName -CommandLine ([string]$p.CommandLine)
      $sumPrivateWs += $privateWs
      $sumPrivateBytes += $privateBytes
      if ($procName -eq "msedgewebview2") { $sumWebviewWs += $privateWs } else { $sumAppWs += $privateWs }
      $lines.Add(("{0},{1},{2},{3},{4},{5},{6},{7},{8},{9},{10}" -f $timeText, $elapsedText, $sampleIndex, $rowPid, ([uint32]$p.ParentProcessId), $kind, (Format-Number ([math]::Round($privateWs, 1)) "0.0"), (Format-Number ([math]::Round($workingSet, 1)) "0.0"), (Format-Number ([math]::Round($privateBytes, 1)) "0.0"), $pageFaults, (Format-Number ([math]::Round($cpuSeconds, 2)) "0.##")))
    }
    Add-CsvLines -Path $processesPath -Header $processesHeader -Lines $lines

    $totalsRow = [pscustomobject]@{
      time = $timeText
      elapsed_min = [math]::Round($elapsedMin, 2)
      sample = $sampleIndex
      processes = $tree.Count
      private_ws_mb = [math]::Round($sumPrivateWs, 1)
      webview2_private_ws_mb = [math]::Round($sumWebviewWs, 1)
      app_private_ws_mb = [math]::Round($sumAppWs, 1)
      private_bytes_mb = [math]::Round($sumPrivateBytes, 1)
      available_mb = [math]::Round($availableMb, 1)
      gap_s = $gapText
    }
    $script:TotalsData.Add($totalsRow)
    Add-CsvLines -Path $totalsPath -Header $totalsHeader -Lines @(("{0},{1},{2},{3},{4},{5},{6},{7},{8},{9}" -f $timeText, $elapsedText, $sampleIndex, $tree.Count, (Format-Number $totalsRow.private_ws_mb "0.0"), (Format-Number $totalsRow.webview2_private_ws_mb "0.0"), (Format-Number $totalsRow.app_private_ws_mb "0.0"), (Format-Number $totalsRow.private_bytes_mb "0.0"), (Format-Number $totalsRow.available_mb "0.0"), $gapText))

    $hours = [int][math]::Floor($elapsedMin / 60.0)
    $minutes = [int][math]::Floor($elapsedMin - $hours * 60.0)
    Write-Host ("[{0:00}:{1:00}] 第 {2} 次：专用工作集 {3} MB（WebView2 {4} MB），{5} 个进程" -f $hours, $minutes, $sampleIndex, (Format-Number $totalsRow.private_ws_mb "0.0"), (Format-Number $totalsRow.webview2_private_ws_mb "0.0"), $tree.Count)
  }
}
finally {
  if ($script:EndedReason -eq "被中断（Ctrl+C 或异常）" -and (Get-Date) -ge $deadline) { $script:EndedReason = "到时" }
  $script:EndedAt = Get-Date

  # 1) 只停本脚本自己启动的进程：先礼后兵，绝不按进程名结束任何进程。
  try {
    if ($script:StartedByScript -and $script:RootPid -gt 0) {
      if ($null -ne (Get-Process -Id $script:RootPid -ErrorAction SilentlyContinue)) {
        # 不带 /F 的 taskkill 只给窗口发 WM_CLOSE，样机会正常退出并写完日志。
        & taskkill.exe /PID $script:RootPid 2>&1 | Out-Null
        try { Wait-Process -Id $script:RootPid -Timeout 15 -ErrorAction SilentlyContinue } catch { }
        if ($null -ne (Get-Process -Id $script:RootPid -ErrorAction SilentlyContinue)) {
          Stop-Process -Id $script:RootPid -Force -ErrorAction SilentlyContinue
        }
      }
      # WebView2 子进程通常随主进程退出；仍残留的只按最后一次采样记录的 PID 结束。
      $childPids = @($script:LastTreePids | Where-Object { $_ -ne [uint32]$script:RootPid })
      $waitDeadline = (Get-Date).AddSeconds(10)
      while ((Get-Date) -lt $waitDeadline) {
        $alive = @($childPids | Where-Object { $null -ne (Get-Process -Id $_ -ErrorAction SilentlyContinue) })
        if ($alive.Count -eq 0) { break }
        Start-Sleep -Milliseconds 500
      }
      foreach ($childPid in $childPids) {
        $left = Get-Process -Id $childPid -ErrorAction SilentlyContinue
        # PID 可能已被系统复用：只结束仍叫 msedgewebview2、且在最后一次采样之前就已启动的进程。
        if ($null -ne $left -and $left.ProcessName -eq "msedgewebview2" -and $null -ne $left.StartTime -and $left.StartTime -le $script:LastSampleAt) {
          Stop-Process -Id $childPid -Force -ErrorAction SilentlyContinue
          $script:LeftoverKilled += $childPid
        }
      }
    }
  } catch { Write-Warning "停止样机时出错：$($_.Exception.Message)" }

  Start-Sleep -Seconds 2

  # 2) 切出样机日志中本次运行新增的部分，顺便统计两个关键事件的出现次数。
  $resuspendedCount = 0
  $settledCount = 0
  try {
    if (Test-Path $panelLog) {
      $allBytes = [System.IO.File]::ReadAllBytes($panelLog)
      $sliceLength = [math]::Max(0, $allBytes.Length - $panelLogStart)
      $slice = [byte[]]::new($sliceLength)
      if ($sliceLength -gt 0) { [Array]::Copy($allBytes, $panelLogStart, $slice, 0, $sliceLength) }
      $slicedLog = Join-Path $OutDir "panel-log.jsonl"
      [System.IO.File]::WriteAllBytes($slicedLog, $slice)
      foreach ($line in [System.IO.File]::ReadAllLines($slicedLog)) {
        if ($line -match '"event":"webview_resuspended"') { $resuspendedCount++ }
        elseif ($line -match '"event":"resuspend_settled"') { $settledCount++ }
      }
    }
  } catch { Write-Warning "切分样机日志时出错：$($_.Exception.Message)" }

  # 3) 摘要，同时打印到控制台。
  try {
    $summaryLines = New-Object System.Collections.Generic.List[string]
    $summaryLines.Add("开始：" + $script:StartedAt.ToString("yyyy-MM-dd HH:mm:ss"))
    $summaryLines.Add("结束：" + $script:EndedAt.ToString("yyyy-MM-dd HH:mm:ss"))
    $summaryLines.Add("结束原因：" + $script:EndedReason)
    $summaryLines.Add("采样：$($script:TotalsData.Count) 次，间隔 $IntervalSeconds 秒，计划 $DurationHours 小时")
    if ($script:GapSeen) { $summaryLines.Add("间隔异常：有采样间隔超过计划间隔的两倍，详见 totals.csv 的 gap_s 列") }
    else { $summaryLines.Add("间隔异常：无") }
    if ($script:TotalsData.Count -gt 0) {
      foreach ($mark in @(@(1, "1 分钟"), @(10, "10 分钟"), @(30, "30 分钟"), @(60, "1 小时"), @(120, "2 小时"), @(240, "4 小时"), @(480, "8 小时"))) {
        $value = Get-ValueAtMinutes -Minutes ([double]$mark[0])
        if ($null -eq $value) { $value = "无数据" } else { $value = "$value MB" }
        $summaryLines.Add("合计专用工作集（$($mark[1])，取最接近的采样点）：$value")
      }
      $last = $script:TotalsData[$script:TotalsData.Count - 1]
      $summaryLines.Add("合计专用工作集（最后一次，第 $($last.sample) 次，$((Format-Number $last.elapsed_min '0.##')) 分钟）：$((Format-Number $last.private_ws_mb '0.0')) MB")
      $minWs = ($script:TotalsData | Measure-Object -Property private_ws_mb -Minimum).Minimum
      $maxWs = ($script:TotalsData | Measure-Object -Property private_ws_mb -Maximum).Maximum
      $summaryLines.Add("最小值：$((Format-Number ([math]::Round($minWs, 1)) '0.0')) MB")
      $summaryLines.Add("最大值：$((Format-Number ([math]::Round($maxWs, 1)) '0.0')) MB")
      $slopeAll = Get-SlopeMbPerHour -FromMinutes 10.0
      if ($null -eq $slopeAll) { $slopeAll = "样本不足" }
      $summaryLines.Add("第 10 分钟到结束的斜率：$slopeAll MB/小时")
      $slopeTail = Get-SlopeMbPerHour -FromMinutes ([math]::Max(0.0, $last.elapsed_min - 120.0))
      if ($null -eq $slopeTail) { $slopeTail = "样本不足" }
      $summaryLines.Add("最后 2 小时的斜率（不足 2 小时取全部）：$slopeTail MB/小时")
    }
    $summaryLines.Add("日志事件：webview_resuspended 出现 $resuspendedCount 次，resuspend_settled 出现 $settledCount 次")
    if ($script:LeftoverKilled.Count -gt 0) {
      $summaryLines.Add("已强制结束的残留 WebView2 进程 PID：" + ($script:LeftoverKilled -join ", "))
    }
    [System.IO.File]::WriteAllText((Join-Path $OutDir "summary.txt"), (($summaryLines -join "`r`n") + "`r`n"), $script:Utf8NoBom)
    $summaryLines | ForEach-Object { Write-Host $_ }
  } catch { Write-Warning "写摘要时出错：$($_.Exception.Message)" }

  # 4) 恢复执行状态。
  if ($script:ExecutionStateSet) {
    [void][Nianlai.Power]::SetThreadExecutionState($esContinuous)
  }
  Write-Host "完成，结果在：$OutDir"
}
