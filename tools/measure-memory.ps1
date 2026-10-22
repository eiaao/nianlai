<#
.SYNOPSIS
  统计拈来进程及其 WebView2 子进程的内存合计（G0 三档内存策略实测用）。

.DESCRIPTION
  从主进程出发，沿父子关系收集整棵进程树（WebView2 的浏览器、渲染、GPU 等进程），
  分别列出专用工作集（任务管理器“内存”列的口径）、工作集和私有提交内存，并给出合计。
  工作集会重复计算多个进程共享的系统库，合计会偏大，比较时以专用工作集为准。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools/measure-memory.ps1 -Name nianlai
#>
param(
  [string]$Name = "nianlai",
  [int]$ProcessId = 0
)

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name
$roots = if ($ProcessId -gt 0) { @($ProcessId) } else { @($all | Where-Object { $_.Name -eq "$Name.exe" } | ForEach-Object { $_.ProcessId }) }
if ($roots.Count -eq 0) { Write-Error "没有找到进程 $Name"; exit 1 }

$tree = New-Object System.Collections.Generic.List[int]
$queue = New-Object System.Collections.Generic.Queue[int]
foreach ($r in $roots) { $queue.Enqueue($r) }
while ($queue.Count -gt 0) {
  $current = $queue.Dequeue()
  if ($tree.Contains($current)) { continue }
  $tree.Add($current)
  $all | Where-Object { $_.ParentProcessId -eq $current } | ForEach-Object { $queue.Enqueue($_.ProcessId) }
}

# 专用工作集和任务管理器“内存”列的口径一致；工作集会重复计算多个进程共享的系统库。
$perf = @{}
Get-CimInstance Win32_PerfFormattedData_PerfProc_Process | ForEach-Object { $perf[[int]$_.IDProcess] = $_.WorkingSetPrivate }
$rows = foreach ($id in $tree) {
  $p = Get-Process -Id $id -ErrorAction SilentlyContinue
  if ($p) {
    [pscustomobject]@{
      进程 = $p.ProcessName
      PID = $id
      专用工作集MB = [math]::Round(($perf[$id] -as [double]) / 1MB, 1)
      工作集MB = [math]::Round($p.WorkingSet64 / 1MB, 1)
      私有提交MB = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
    }
  }
}
$rows | Format-Table -AutoSize | Out-String -Width 200 | Write-Output
$pws = ($rows | Measure-Object -Property 专用工作集MB -Sum).Sum
$ws = ($rows | Measure-Object -Property 工作集MB -Sum).Sum
$pm = ($rows | Measure-Object -Property 私有提交MB -Sum).Sum
Write-Output ("合计：{0} 个进程，专用工作集 {1} MB，工作集 {2} MB，私有提交 {3} MB" -f $rows.Count, [math]::Round($pws, 1), [math]::Round($ws, 1), [math]::Round($pm, 1))
