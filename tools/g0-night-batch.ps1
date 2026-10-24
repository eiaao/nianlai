<#
.SYNOPSIS
  夜间无人值守批处理（G0）：按顺序跑焦点矩阵（空跑 → 冒烟 → 全量）和内存基线（默认 8 小时），逐步写 batch.log，收尾写 summary.txt。

.DESCRIPTION
  给“用户睡觉、早上别人分析结果”的一整晚编排，共 9 步：
    1  前置检查：已有 nianlai.exe 在运行时立即停止并提示先按 Ctrl+Alt+Shift+Q 退出样机；
       只读环境检查（UU 远程、记事本、资源管理器文件夹窗口、屏保、输入法、显示器）写进日志和 run.json；
       默认输入法不是英文键盘（默认 TIP 不含 :00000409）时在日志和控制台给醒目警告，不阻断；
       调用 SetThreadExecutionState(ES_CONTINUOUS|ES_SYSTEM_REQUIRED|ES_DISPLAY_REQUIRED) 阻止睡眠并保持
       显示器开着，结束或出错时恢复。run.json 延后到步骤 2 结束再写（要记 rebuilt 等只有重建后才知道的字段）。
    2  重建样机：cmd.exe /d /c pnpm tauri build --no-bundle；stdout 存 <Out>\build-prototype.log、stderr 存
       <Out>\build-prototype.stderr.txt，预计 5–8 分钟（正式版胖 LTO）；-SkipBuild 跳过。
       失败时若旧样机仍存在就沿用旧 exe（Notes 记“样机未重建，沿用旧 exe”）；样机不存在则跳过矩阵和
       内存，直接收尾，批处理退出码 1。
    3  cargo build -q -p focus-matrix，之后直接运行 target\debug\focus-matrix.exe；失败跳过矩阵，直接进步骤 7。
    4  矩阵空跑 --dry-run --json，stdout 存 <Out>\matrix-dryrun.json；退出码非 0 跳过矩阵，进步骤 7。
       空跑 JSON 里的 blockers、estimated_minutes、upper_minutes 和每个场景的 id/status 抄进日志和汇总。
    5  冒烟 --real-input --only helper --trials 4（覆盖数字、Enter、数字、Esc 的完整轮换）；stdout 存
       <Out>\matrix-smoke.json、进度（stderr）存 <Out>\matrix-smoke.progress.txt；非 0 跳过全量，进步骤 7。
    6  全量 --real-input --trials <MatrixTrials> --max-minutes <MatrixMaxMinutes>；stdout 存 <Out>\matrix-full.json、
       进度存 <Out>\matrix-full.progress.txt；不管成败都继续。
    7  清理和等待：等最多 120 秒直到没有 nianlai.exe；仍有的只结束启动时间晚于批处理开始的（按 PID，
       Stop-Process -Id）。
    8  内存基线：进入前先把执行状态降为 ES_CONTINUOUS|ES_SYSTEM_REQUIRED（矩阵阶段已结束，显示器按电源
       计划处理，系统仍不睡眠）；之后 powershell -NoProfile -ExecutionPolicy Bypass -File
       tools\measure-memory-overnight.ps1 -Exe <Exe> -ResuspendMinutes 0 -DurationHours <MemoryHours>
       -Label baseline -OutDir <Out>\memory-baseline。
    9  写 <Out>\summary.txt：每步结果、退出码、耗时、产出目录；抄入矩阵空跑摘要、矩阵全量的 summary.md
       和内存 summary.txt 的前 30 行；恢复睡眠设置。
  每步开始、结束、退出码、耗时都带时间戳追加进 <Out>\batch.log 并同时打印到控制台。
  子进程一律 Start-Process -PassThru + WaitForExit()，不用 -Wait：PS 5.1 的 -Wait 会等整棵进程树退出，
  矩阵工具留下的浏览器、WebView2 或崩溃处理进程会让批处理整夜卡住。
  矩阵真跑会接管键盘鼠标，运行期间不要动鼠标键盘；详见 tools\focus-matrix\README.md。

.PARAMETER Exe
  被测样机路径，默认 <仓库>\target\release\nianlai.exe。

.PARAMETER Out
  输出目录，默认 <仓库>\target\g0-night\<yyyyMMdd-HHmm>。目录已存在时日志会向其中追加。

.PARAMETER MatrixTrials
  矩阵全量每个场景每种模式的次数，默认 30。

.PARAMETER MatrixMaxMinutes
  矩阵全量的总时限（分钟，含倒计时和准备阶段），默认 200（工具自己估的全量上界是 190 分钟，
  150 会在慢的时候把全量截断）。

.PARAMETER MemoryHours
  内存基线实验时长（小时），默认 8。

.PARAMETER SkipBuild
  跳过步骤 2（重建样机），直接用当前的 target\release\nianlai.exe。

.PARAMETER SkipMatrix
  跳过步骤 3–7（矩阵部分），直接做内存基线。

.PARAMETER DryRun
  只做步骤 1 的检查（有样机在跑时报告出来但不当成错误退出；UU 远程、记事本、资源管理器窗口、
  屏保等同样只提醒不阻断），打印每一步将要执行的完整命令和预计耗时；不建输出目录、不构建、
  不运行任何东西。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\g0-night-batch.ps1 -DryRun
  睡前先看计划：每步的命令和预计耗时，以及样机、git、输入法、显示器的检查结果。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\g0-night-batch.ps1
  默认跑法：重建样机 → 矩阵（空跑 → 冒烟 → 全量，上限 200 分钟）→ 清理 → 内存基线 8 小时。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\g0-night-batch.ps1 -SkipMatrix -MemoryHours 6
  跳过矩阵，只跑 6 小时内存基线。
#>
[CmdletBinding()]
param(
  [string]$Exe = "",
  [string]$Out = "",
  [int]$MatrixTrials = 30,
  [int]$MatrixMaxMinutes = 200,
  [double]$MemoryHours = 8,
  [switch]$SkipBuild,
  [switch]$SkipMatrix,
  [switch]$DryRun
)

try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$script:Invariant = [System.Globalization.CultureInfo]::InvariantCulture

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($Exe)) { $Exe = Join-Path $repoRoot "target\release\nianlai.exe" }
if ([string]::IsNullOrWhiteSpace($Out)) { $Out = Join-Path $repoRoot ("target\g0-night\{0:yyyyMMdd-HHmm}" -f (Get-Date)) }
$matrixExe = Join-Path $repoRoot "target\debug\focus-matrix.exe"
$memoryScript = Join-Path $repoRoot "tools\measure-memory-overnight.ps1"
$buildLog = Join-Path $Out "build-prototype.log"
$buildStderr = Join-Path $Out "build-prototype.stderr.txt"
$matrixSmokeDir = Join-Path $Out "matrix-smoke"
$matrixFullDir = Join-Path $Out "matrix-full"
$matrixSmokeJson = Join-Path $Out "matrix-smoke.json"
$matrixSmokeProgress = Join-Path $Out "matrix-smoke.progress.txt"
$matrixFullJson = Join-Path $Out "matrix-full.json"
$matrixFullProgress = Join-Path $Out "matrix-full.progress.txt"
$memoryOutDir = Join-Path $Out "memory-baseline"

$script:BatchLogPath = ""   # 步骤 1 创建输出目录后才有值；为空时 Write-Log 只打印不落盘
$script:BatchStart = [datetime]::MinValue
$script:ExecutionStateSet = $false
$script:GuardReady = $false
$script:Steps = New-Object System.Collections.Generic.List[object]
$script:Notes = New-Object System.Collections.Generic.List[string]
$script:MatrixDryrunLines = New-Object System.Collections.Generic.List[string]   # 步骤 4 空跑后填，收尾进汇总

# ---------- Ctrl+C 兜底 ----------
# PowerShell 5.1 收到 Ctrl+C 停止管道时不执行脚本里的 finally，这里用 C# 静态构造挂
# Console.CancelKeyPress：回调只做三件事——写日志、按 PID 结束本脚本自己启动的子进程
# （先核对进程名，避免 PID 复用误杀）和启动时间晚于批处理开始的样机、恢复睡眠设置。
# 回调在 .NET 线程上运行，不碰 PowerShell 的 Runspace。即便它没被触发，进程退出时系统
# 也会自动释放执行状态请求，最坏只留下孤儿子进程。
$script:GuardSource = @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

public static class NianlaiNightGuard {
  [DllImport("kernel32.dll")]
  private static extern uint SetThreadExecutionState(uint esFlags);

  private static readonly object Gate = new object();
  private static bool cancelHandled = false;
  private static readonly List<int> childPids = new List<int>();
  private static readonly List<string> childNames = new List<string>();
  private static string logPath = "";
  private static DateTime batchStartLocal = DateTime.MinValue;

  public static void Enable(string path, DateTime batchStart) {
    lock (Gate) { logPath = path; batchStartLocal = batchStart; }
  }

  public static void TrackChild(int pid, string processName) {
    lock (Gate) { childPids.Add(pid); childNames.Add(processName == null ? "" : processName); }
  }

  public static bool SetExecutionState(uint flags) {
    return SetThreadExecutionState(flags) != 0;
  }

  static NianlaiNightGuard() {
    Console.CancelKeyPress += new ConsoleCancelEventHandler(OnCancelKeyPress);
  }

  private static void OnCancelKeyPress(object sender, ConsoleCancelEventArgs e) {
    List<int> pids; List<string> names; string path; DateTime start;
    lock (Gate) {
      if (cancelHandled) { return; }
      cancelHandled = true;
      pids = new List<int>(childPids);
      names = new List<string>(childNames);
      path = logPath;
      start = batchStartLocal;
    }
    List<string> lines = new List<string>();
    lines.Add("收到 Ctrl+C：开始清理本脚本启动的进程。");
    for (int i = 0; i < pids.Count; i++) {
      try {
        Process p = Process.GetProcessById(pids[i]);
        if (p.ProcessName != names[i]) {
          lines.Add("PID " + pids[i] + " 已被复用成 " + p.ProcessName + "，不动它。");
          continue;
        }
        if (!p.HasExited) {
          p.Kill();
          lines.Add("已结束子进程 PID " + pids[i] + "（" + p.ProcessName + "）。");
        }
      } catch (ArgumentException) {
        // PID 已经不存在。
      } catch (Exception ex) {
        lines.Add("结束子进程 PID " + pids[i] + " 失败：" + ex.Message);
      }
    }
    // 只结束启动时间晚于批处理开始的样机：批处理开始时已确认没有样机，
    // 之后出现的 nianlai.exe 只能是矩阵工具或内存脚本启动的。
    try {
      Process[] all = Process.GetProcessesByName("nianlai");
      foreach (Process p in all) {
        try {
          if (start != DateTime.MinValue && p.StartTime > start) {
            string startedText = p.StartTime.ToString("yyyy-MM-dd HH:mm:ss");
            p.Kill();
            lines.Add("已结束样机 PID " + p.Id + "（启动于 " + startedText + "，晚于批处理开始）。");
          }
        } catch (Exception) { }
      }
    } catch (Exception) { }
    SetThreadExecutionState(0x80000000u);
    lines.Add("已恢复系统睡眠设置（SetThreadExecutionState(ES_CONTINUOUS)）。");
    try {
      if (!string.IsNullOrEmpty(path)) {
        StringBuilder sb = new StringBuilder();
        foreach (string line in lines) {
          sb.Append("[").Append(DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss")).Append("] ").Append(line).Append("\r\n");
        }
        File.AppendAllText(path, sb.ToString(), new UTF8Encoding(false));
      }
    } catch (Exception) { }
  }
}
'@

# ---------- 小工具 ----------

function Write-Log {
  # 追加进 batch.log 并打印到控制台；DryRun 或日志路径未就绪时只打印。
  param([string]$Message, [switch]$Warn)
  $line = "[{0:yyyy-MM-dd HH:mm:ss}] {1}" -f (Get-Date), $Message
  if ($Warn) { Write-Host $line -ForegroundColor Yellow } else { Write-Host $line }
  if ($script:BatchLogPath -ne "") {
    try { [System.IO.File]::AppendAllText($script:BatchLogPath, $line + "`r`n", $script:Utf8NoBom) } catch { }
  }
}

function Join-CommandLine {
  # 拼出给人看的完整命令行；含空白的参数加引号。
  param([string]$FilePath, [string[]]$ArgumentList)
  $parts = New-Object System.Collections.Generic.List[string]
  $parts.Add($FilePath)
  if ($null -ne $ArgumentList) { foreach ($a in $ArgumentList) { $parts.Add([string]$a) } }
  (@($parts | ForEach-Object { if ($_ -match "\s") { '"' + $_ + '"' } else { $_ } }) -join " ")
}

function Format-DurationText {
  param([double]$Seconds)
  if ($Seconds -lt 90.0) { return ("{0:n1} 秒" -f $Seconds) }
  if ($Seconds -lt 5400.0) { return ("{0:n1} 分钟" -f ($Seconds / 60.0)) }
  return ("{0:n1} 小时" -f ($Seconds / 3600.0))
}

function Show-Plan {
  # DryRun 专用：打印一步计划。
  param([string]$Title, [string]$Command, [string]$Estimate, [string]$Note = "")
  Write-Host ""
  Write-Host ("[计划] " + $Title)
  if ($Command -ne "") { Write-Host ("       命令：" + $Command) }
  Write-Host ("       预计：" + $Estimate)
  if ($Note -ne "") { Write-Host ("       说明：" + $Note) }
}

function Get-GitInfo {
  $r = [pscustomobject]@{ Head = ""; Status = @(); Available = $false }
  try {
    $head = & git -C $repoRoot rev-parse HEAD 2>$null
    if ($LASTEXITCODE -eq 0 -and $head) {
      $r.Head = [string]$head
      $r.Available = $true
      $st = @(& git -C $repoRoot status --short 2>$null)
      $r.Status = @($st | Where-Object { $_ })
    }
  } catch { }
  return $r
}

function Get-InputMethodInfo {
  $errors = New-Object System.Collections.Generic.List[string]
  $overrideTip = ""
  try {
    $o = Get-WinDefaultInputMethodOverride
    # Windows PowerShell 5.1 返回的对象属性是单数 InputMethodTip；复数形式只作兜底。
    if ($null -ne $o -and $null -ne $o.InputMethodTip) { $overrideTip = [string]$o.InputMethodTip }
    elseif ($null -ne $o -and $null -ne $o.InputMethodTips) { $overrideTip = [string]$o.InputMethodTips }
  } catch { $errors.Add("Get-WinDefaultInputMethodOverride 失败：" + $_.Exception.Message) }
  $tips = New-Object System.Collections.Generic.List[string]
  try {
    foreach ($lang in @(Get-WinUserLanguageList)) {
      foreach ($tip in @($lang.InputMethodTips)) {
        if ($tip) { $tips.Add([string]$tip) }
      }
    }
  } catch { $errors.Add("Get-WinUserLanguageList 失败：" + $_.Exception.Message) }
  $defaultTip = $overrideTip
  if ([string]::IsNullOrWhiteSpace($defaultTip) -and $tips.Count -gt 0) { $defaultTip = $tips[0] }
  # 判定规则：默认 TIP 的布局 ID 是 00000409（美式英文键盘）即视为英文键盘；
  # 微软拼音等中文 IME 的 TIP 是 0804:xxxx，不满足。只用于提醒，不参与阻断。
  $isEnglish = ($defaultTip -match ":00000409")
  return [pscustomobject]@{
    OverrideTip = $overrideTip
    LanguageTips = @($tips)
    DefaultTip = $defaultTip
    IsEnglishKeyboard = [bool]$isEnglish
    Errors = @($errors)
  }
}

function Get-MonitorCount {
  try {
    Add-Type -AssemblyName System.Windows.Forms
    return [int]([System.Windows.Forms.Screen]::AllScreens.Count)
  } catch { return -1 }
}

function Get-HeadCommitTimeIso {
  # HEAD 提交时间（git log -1 --format=%cI，ISO 8601 带时区）；git 不可用时返回空字符串。
  try {
    $t = & git -C $repoRoot log -1 --format=%cI 2>$null
    if ($LASTEXITCODE -eq 0 -and $t) { return [string]@($t)[0] }
  } catch { }
  return ""
}

function Get-EnvironmentChecks {
  # 只读环境检查，全部只警告、不阻断：网易 UU 远程、记事本、资源管理器文件夹窗口、屏保。
  # 不读窗口标题和路径，不接触剪贴板；查询失败只记错误，记 -1。
  $errors = New-Object System.Collections.Generic.List[string]
  $gameViewerCount = -1
  $notepadCount = -1
  $explorerWindows = -1
  $screensaverLock = $false
  try { $gameViewerCount = @(Get-Process -Name "GameViewer" -ErrorAction SilentlyContinue).Count } catch { $errors.Add("查询 GameViewer.exe 失败：" + $_.Exception.Message) }
  try { $notepadCount = @(Get-Process -Name "Notepad" -ErrorAction SilentlyContinue).Count } catch { $errors.Add("查询 Notepad.exe 失败：" + $_.Exception.Message) }
  try {
    # 只统计 FullName 以 explorer.exe 结尾的窗口个数，不读窗口标题和路径。
    $shell = New-Object -ComObject Shell.Application
    $count = 0
    foreach ($w in @($shell.Windows())) {
      $fn = ""
      try { $fn = [string]$w.FullName } catch { $fn = "" }
      if ($fn -like "*explorer.exe") { $count++ }
    }
    $explorerWindows = $count
  } catch { $errors.Add("Shell.Application.Windows() 枚举资源管理器窗口失败：" + $_.Exception.Message) }
  try {
    $desk = Get-ItemProperty -Path "HKCU:\Control Panel\Desktop" -ErrorAction Stop
    $scrnSave = ""
    if ($null -ne $desk.'SCRNSAVE.EXE') { $scrnSave = [string]$desk.'SCRNSAVE.EXE' }
    $secure = ""
    if ($null -ne $desk.ScreenSaverIsSecure) { $secure = [string]$desk.ScreenSaverIsSecure }
    $screensaverLock = ($scrnSave.Trim() -ne "" -and $secure.Trim() -eq "1")
  } catch { $errors.Add("读取屏保设置（HKCU\Control Panel\Desktop）失败：" + $_.Exception.Message) }
  return [pscustomobject]@{
    GameViewerCount = [int]$gameViewerCount
    NotepadCount = [int]$notepadCount
    ExplorerFolderWindows = [int]$explorerWindows
    ScreensaverLockEnabled = [bool]$screensaverLock
    Errors = @($errors)
  }
}

function Format-EnvironmentChecks {
  # 把环境检查的结果整理成“文本 + 是否警告”的行列表，DryRun 与正式模式共用。
  param([pscustomobject]$Checks)
  $lines = New-Object System.Collections.Generic.List[object]
  if ($Checks.GameViewerCount -gt 0) {
    $lines.Add([pscustomobject]@{
      Text = ("检测到网易 UU 远程 GameViewer.exe 在运行（" + $Checks.GameViewerCount + " 个）：它会抢先读取并回写剪贴板，可能给矩阵的粘贴核对加噪声，建议睡前退出")
      Warn = $true
    })
  } elseif ($Checks.GameViewerCount -eq 0) {
    $lines.Add([pscustomobject]@{ Text = "没有发现网易 UU 远程 GameViewer.exe 在运行"; Warn = $false })
  } else {
    $lines.Add([pscustomobject]@{ Text = "查询 GameViewer.exe 失败（run.json 里记 -1）"; Warn = $true })
  }
  if ($Checks.NotepadCount -gt 0) {
    $lines.Add([pscustomobject]@{
      Text = ("检测到记事本 Notepad.exe 在运行（" + $Checks.NotepadCount + " 个）：矩阵 README 要求先关闭记事本，它会复用旧窗口的标签页，建议睡前关闭")
      Warn = $true
    })
  } elseif ($Checks.NotepadCount -eq 0) {
    $lines.Add([pscustomobject]@{ Text = "没有发现记事本 Notepad.exe 在运行"; Warn = $false })
  } else {
    $lines.Add([pscustomobject]@{ Text = "查询 Notepad.exe 失败（run.json 里记 -1）"; Warn = $true })
  }
  if ($Checks.ExplorerFolderWindows -gt 0) {
    $lines.Add([pscustomobject]@{
      Text = ("资源管理器打开了 " + $Checks.ExplorerFolderWindows + " 个文件夹窗口：建议关闭（只统计了个数，不读窗口标题和路径）")
      Warn = $true
    })
  } elseif ($Checks.ExplorerFolderWindows -eq 0) {
    $lines.Add([pscustomobject]@{ Text = "资源管理器没有打开的文件夹窗口"; Warn = $false })
  } else {
    $lines.Add([pscustomobject]@{ Text = "查询资源管理器文件夹窗口失败（run.json 里记 -1）"; Warn = $true })
  }
  if ($Checks.ScreensaverLockEnabled) {
    $lines.Add([pscustomobject]@{
      Text = "启用了带锁定的屏幕保护（SCRNSAVE.EXE 已设置且 ScreenSaverIsSecure=1）：SetThreadExecutionState 拦不住屏保，屏保一锁屏矩阵就失败，建议睡前关掉屏保"
      Warn = $true
    })
  } else {
    $lines.Add([pscustomobject]@{ Text = "没有启用带锁定的屏幕保护"; Warn = $false })
  }
  foreach ($err in @($Checks.Errors)) { $lines.Add([pscustomobject]@{ Text = $err; Warn = $true }) }
  return $lines
}

function Invoke-StepCommand {
  # 运行一个外部命令并完整记录开始/结束/退出码/耗时；返回退出码。
  # 不用 Start-Process -Wait：PS 5.1 的 -Wait 把子进程放进作业对象，要等整棵进程树都退出才返回，
  # 矩阵工具留下的浏览器、WebView2 或崩溃处理进程会让批处理整夜卡在这一步。
  param(
    [string]$StepName,
    [string]$FilePath,
    [string[]]$ArgumentList,
    [string]$WorkingDirectory = "",
    [string]$RedirectStdout = "",
    [string]$RedirectStderr = "",
    [string]$OutputDesc = ""
  )
  if ([string]::IsNullOrEmpty($WorkingDirectory)) { $WorkingDirectory = $repoRoot }
  $cmdText = Join-CommandLine -FilePath $FilePath -ArgumentList $ArgumentList
  if ($RedirectStdout -ne "") { $cmdText = $cmdText + ' 1> "' + $RedirectStdout + '"' }
  if ($RedirectStderr -ne "") { $cmdText = $cmdText + ' 2> "' + $RedirectStderr + '"' }
  Write-Log ("{0}：开始" -f $StepName)
  Write-Log ("命令：{0}" -f $cmdText)
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $code = -1
  $failText = ""
  try {
    $spArgs = @{
      FilePath = $FilePath; ArgumentList = $ArgumentList; WorkingDirectory = $WorkingDirectory
      NoNewWindow = $true; PassThru = $true
    }
    if ($RedirectStdout -ne "") { $spArgs["RedirectStandardOutput"] = $RedirectStdout }
    if ($RedirectStderr -ne "") { $spArgs["RedirectStandardError"] = $RedirectStderr }
    $proc = Start-Process @spArgs
    if ($null -ne $proc -and $proc.Id -gt 0) {
      # 立刻缓存本机句柄：PS 5.1 不缓存的话进程退出后 ExitCode 可能拿不到。
      try { $null = $proc.Handle } catch { }
      # 子进程仍在运行时就登记 PID 和进程名，Ctrl+C 回调才来得及按 PID 清理自己启动的子进程。
      # 进程名取自文件名而不是 $proc.ProcessName：子进程退出得快时读 ProcessName 会抛异常，把成功的步骤误记成 -1。
      if ($script:GuardReady) {
        try { [NianlaiNightGuard]::TrackChild($proc.Id, [System.IO.Path]::GetFileNameWithoutExtension($FilePath)) } catch { }
      }
      $proc.WaitForExit()
      $code = $proc.ExitCode
      if ($null -eq $code) { $proc.Refresh(); $code = $proc.ExitCode }
      if ($null -eq $code) { $failText = "拿不到退出码，按 -1 记"; $code = -1 }
    } elseif ($null -ne $proc) {
      $failText = "Start-Process 返回的进程没有 PID，按 -1 记"
    } else {
      $failText = "Start-Process 没有返回进程对象，按 -1 记"
    }
  } catch {
    $failText = "启动失败：" + $_.Exception.Message
    $code = -1
  }
  $sw.Stop()
  if ($failText -ne "") { Write-Log ("{0}：{1}" -f $StepName, $failText) -Warn }
  Write-Log ("{0}：结束，退出码 {1}，耗时 {2}" -f $StepName, $code, (Format-DurationText $sw.Elapsed.TotalSeconds))
  $script:Steps.Add([pscustomobject]@{
    name = $StepName
    command = $cmdText
    exit_code = $code
    seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
    output = $OutputDesc
  })
  return $code
}

# ---------- DryRun：只做第 1 步的检查并打印计划 ----------

if ($DryRun) {
  Write-Host "==== DryRun：只做第 1 步的检查并打印计划；不创建目录、不构建、不运行任何东西 ===="
  Write-Host ("仓库：" + $repoRoot)
  Write-Host ("输出目录（DryRun 不创建）：" + $Out)
  Write-Host ""
  # HEAD 提交时间：判断当前样机是否早于 HEAD（早于则正式运行会先重建）。
  $headTimeIso = Get-HeadCommitTimeIso
  $headDto = $null
  if ($headTimeIso -ne "") {
    try { $headDto = [DateTimeOffset]::Parse($headTimeIso, $script:Invariant, [System.Globalization.DateTimeStyles]::RoundtripKind) } catch { $headDto = $null }
  }
  Write-Host "[检查] 样机"
  if (Test-Path -LiteralPath $Exe -PathType Leaf) {
    $item = Get-Item -LiteralPath $Exe
    $sha = ""
    try { $sha = (Get-FileHash -LiteralPath $Exe -Algorithm SHA256).Hash } catch { $sha = "" }
    $shaText = "（计算失败）"
    if ($sha -ne "") { $shaText = $sha.Substring(0, [Math]::Min(16, $sha.Length)) + "…（完整值写进正式运行的 run.json）" }
    Write-Host ("  " + $Exe)
    Write-Host ("  存在：" + $item.Length + " 字节，修改时间 " + $item.LastWriteTime.ToString("yyyy-MM-dd HH:mm:ss") + "，SHA-256 " + $shaText)
    if ($null -ne $headDto -and [DateTimeOffset]$item.LastWriteTime -lt $headDto) {
      Write-Host ("  【提示】样机早于 HEAD，正式运行会先重建（除非 -SkipBuild）。HEAD 提交时间：" + $headDto.LocalDateTime.ToString("yyyy-MM-dd HH:mm:ss")) -ForegroundColor Yellow
    }
  } else {
    Write-Host ("  【警告】找不到样机：" + $Exe + "；正式运行时步骤 2 会先重建，构建仍失败则跳过矩阵和内存并退出 1") -ForegroundColor Yellow
  }
  Write-Host "[检查] 样机进程"
  $running = @(Get-Process -Name "nianlai" -ErrorAction SilentlyContinue)
  if ($running.Count -gt 0) {
    $ids = (@($running | ForEach-Object { $_.Id }) -join ", ")
    Write-Host ("  【警告】发现 nianlai.exe 正在运行（PID " + $ids + "）。正式运行会在第 1 步停止并提示：请先按 Ctrl+Alt+Shift+Q 退出样机。") -ForegroundColor Yellow
  } else {
    Write-Host "  当前没有 nianlai.exe 在运行"
  }
  Write-Host "[检查] git"
  $git = Get-GitInfo
  if ($git.Available) {
    Write-Host ("  HEAD " + $git.Head)
    if ($headTimeIso -ne "") {
      Write-Host ("  HEAD 提交时间：" + $headTimeIso)
    } else {
      Write-Host "  【警告】拿不到 HEAD 提交时间（git log）；正式运行的 run.json 里 head_commit_time 记空、older_than_head 记 false" -ForegroundColor Yellow
    }
    if ($git.Status.Count -eq 0) {
      Write-Host "  工作区干净"
    } else {
      Write-Host ("  工作区有 " + $git.Status.Count + " 条改动：")
      foreach ($s in $git.Status) { Write-Host ("    " + $s) }
    }
  } else {
    Write-Host "  【警告】git 不可用或不在仓库内；正式运行会在 run.json 里记为空" -ForegroundColor Yellow
  }
  Write-Host "[检查] 输入法"
  $im = Get-InputMethodInfo
  if ($im.OverrideTip) {
    Write-Host ("  默认输入法覆盖（Get-WinDefaultInputMethodOverride）：" + $im.OverrideTip)
  } else {
    Write-Host "  没有设置默认输入法覆盖，取语言列表第一个 TIP 作默认"
  }
  Write-Host ("  语言列表 InputMethodTips：" + (@($im.LanguageTips) -join " | "))
  if ($im.DefaultTip) { Write-Host ("  判定用的默认 TIP：" + $im.DefaultTip) }
  if (-not $im.IsEnglishKeyboard) {
    Write-Host "  【警告】默认输入法不是英文键盘。矩阵搜索首字用当前键盘布局的普通虚拟键触发，中文组合输入可能让首字尚未成为文本（见 tools\focus-matrix\README.md）；建议睡前把「替代默认输入法」设为「中文(简体，中国) - 美式键盘」（TIP 0804:00000409）。正式运行时同样只警告，不阻断。" -ForegroundColor Yellow
  } else {
    Write-Host "  默认输入法是英文键盘"
  }
  foreach ($err in @($im.Errors)) { Write-Host ("  " + $err) -ForegroundColor Yellow }
  Write-Host "[检查] 显示器"
  $monitors = Get-MonitorCount
  if ($monitors -gt 0) {
    $extra = ""
    if ($monitors -eq 1) { $extra = "（单屏：矩阵的第二屏场景会被跳过，不阻断）" }
    Write-Host ("  " + $monitors + " 块" + $extra)
  } else {
    Write-Host "  【警告】查询显示器数失败；正式运行会在 run.json 里记 -1" -ForegroundColor Yellow
  }
  Write-Host "[检查] 环境干扰（只提醒，不阻断）"
  $envChecks = Get-EnvironmentChecks
  foreach ($l in (Format-EnvironmentChecks -Checks $envChecks)) {
    if ($l.Warn) { Write-Host ("  " + $l.Text) -ForegroundColor Yellow } else { Write-Host ("  " + $l.Text) }
  }

  Write-Host ""
  Write-Host "---- 今晚计划 ----"
  Write-Host "[计划] 步骤 1/9：前置检查——只读环境检查（UU 远程、记事本、资源管理器窗口、屏保、输入法、显示器）写日志；SetThreadExecutionState 阻止睡眠并保持显示器开；run.json 在步骤 2 后写（要记 rebuilt 等构建字段）"
  $memHoursText = $MemoryHours.ToString("0.##", $script:Invariant)
  if ($SkipBuild) {
    Show-Plan -Title "步骤 2/9：重建样机" -Command "" -Estimate "0" -Note "按 -SkipBuild 跳过，直接用当前样机；run.json 里 rebuilt 记 skipped"
  } else {
    Show-Plan -Title "步骤 2/9：重建样机" -Command ((Join-CommandLine "cmd.exe" @("/d", "/c", "pnpm tauri build --no-bundle")) + ' 1> "' + $buildLog + '" 2> "' + $buildStderr + '"') -Estimate "约 5–8 分钟（正式版胖 LTO）" -Note "失败时若旧样机仍存在就沿用旧 exe；旧样机也不存在则跳过矩阵和内存，直接收尾，退出码 1"
  }
  if ($SkipMatrix) {
    Show-Plan -Title "步骤 3–7/9：矩阵部分" -Command "" -Estimate "0" -Note "按 -SkipMatrix 跳过（构建、空跑、冒烟、全量、清理等待都不执行）"
  } else {
    $cargoCmd = Get-Command cargo -ErrorAction SilentlyContinue
    if ($null -eq $cargoCmd) {
      Write-Host ""
      Write-Host "[计划] 步骤 3/9：构建矩阵工具——【警告】PATH 里没有 cargo，正式运行会跳过矩阵（步骤 4–6），直接进步骤 7" -ForegroundColor Yellow
    } else {
      Show-Plan -Title "步骤 3/9：构建矩阵工具" -Command (Join-CommandLine $cargoCmd.Source @("build", "-q", "-p", "focus-matrix")) -Estimate "约 0.5–3 分钟（视 cargo 缓存）" -Note "失败则跳过步骤 4–6，直接进步骤 7"
    }
    Show-Plan -Title "步骤 4/9：矩阵空跑（只读检查）" -Command ((Join-CommandLine $matrixExe @("--dry-run", "--json", "--exe", $Exe)) + ' 1> "' + (Join-Path $Out "matrix-dryrun.json") + '" 2> "' + (Join-Path $Out "matrix-dryrun.stderr.txt") + '"') -Estimate "1 分钟以内" -Note "退出码非 0（空跑发现阻断）则跳过步骤 5–6，直接进步骤 7；空跑 JSON 的 blockers、时长估计和每个场景的 id/status 会抄进日志和汇总"
    Show-Plan -Title "步骤 5/9：矩阵冒烟（真实输入）" -Command ((Join-CommandLine $matrixExe @("--real-input", "--only", "helper", "--trials", "4", "--exe", $Exe, "--out", $matrixSmokeDir)) + ' 1> "' + $matrixSmokeJson + '" 2> "' + $matrixSmokeProgress + '"') -Estimate "约 3–8 分钟（helper 场景 4 次 × 快速/搜索 2 模式，另有 10 秒倒计时）" -Note "trials=4 覆盖数字、Enter、数字、Esc 的完整轮换；退出码非 0 则跳过步骤 6"
    Show-Plan -Title "步骤 6/9：矩阵全量（真实输入）" -Command ((Join-CommandLine $matrixExe @("--real-input", "--trials", [string]$MatrixTrials, "--max-minutes", [string]$MatrixMaxMinutes, "--exe", $Exe, "--out", $matrixFullDir)) + ' 1> "' + $matrixFullJson + '" 2> "' + $matrixFullProgress + '"') -Estimate ("上限 " + $MatrixMaxMinutes + " 分钟（--max-minutes 是硬上界）") -Note ("10 个默认启用场景（wps 默认禁用）× 2 模式 × " + $MatrixTrials + " 次；不管成败都继续步骤 7")
    Show-Plan -Title "步骤 7/9：清理和等待" -Command "" -Estimate "最多 120 秒" -Note "等没有 nianlai.exe 为止；仍有的只结束启动时间晚于批处理开始的（按 PID，Stop-Process -Id）"
  }
  $memCmd = Join-CommandLine "powershell.exe" (@("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $memoryScript, "-Exe", $Exe, "-ResuspendMinutes", "0", "-DurationHours", $memHoursText, "-Label", "baseline", "-OutDir", $memoryOutDir))
  Show-Plan -Title "步骤 8/9：内存基线" -Command $memCmd -Estimate ("约 " + $memHoursText + " 小时（-DurationHours）") -Note "进入前先把执行状态降为 ES_CONTINUOUS|ES_SYSTEM_REQUIRED：矩阵阶段已结束，显示器按电源计划处理，系统仍不睡眠；它自己会启动样机并在结束时收尾，等待它结束"
  Show-Plan -Title "步骤 9/9：收尾" -Command "" -Estimate "几秒" -Note ("写 " + (Join-Path $Out "summary.txt") + "（每步结果 + 空跑摘要 + 矩阵 summary.md + 内存 summary.txt 前 30 行），恢复睡眠设置")
  $planHours = $MemoryHours + 0.1
  if (-not $SkipBuild) { $planHours = $planHours + 8.0 / 60.0 }
  if (-not $SkipMatrix) { $planHours = $planHours + $MatrixMaxMinutes / 60.0 + 0.2 }
  Write-Host ""
  Write-Host ("[计划] 总计：最长约 " + $planHours.ToString("0.0", $script:Invariant) + " 小时（构建按 8 分钟、矩阵部分按 --max-minutes 上界估计，实际通常更短）")
  Write-Host "==== DryRun 结束：没有创建任何目录、没有运行任何命令 ===="
  exit 0
}

# ================= 正式模式：步骤 1/9 前置检查 =================

$running = @(Get-Process -Name "nianlai" -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
  $ids = (@($running | ForEach-Object { $_.Id }) -join ", ")
  Write-Host ("已有 nianlai.exe 在运行（PID " + $ids + "）。请先按 Ctrl+Alt+Shift+Q 退出样机，再运行本脚本。") -ForegroundColor Red
  exit 1
}
New-Item -ItemType Directory -Path $Out -Force | Out-Null
$script:BatchLogPath = Join-Path $Out "batch.log"
$script:BatchStart = Get-Date

Write-Log "================ 拈来 G0 夜间批处理 ================"
Write-Log ("仓库：" + $repoRoot)
Write-Log ("样机：" + $Exe)
Write-Log ("输出目录：" + $Out)
Write-Log ("参数：MatrixTrials=$MatrixTrials，MatrixMaxMinutes=$MatrixMaxMinutes，MemoryHours=" + $MemoryHours.ToString("0.##", $script:Invariant) + "，SkipBuild=$SkipBuild，SkipMatrix=$SkipMatrix")

# git、环境、输入法、显示器（只读，逐项容错）。样机的 SHA-256/大小/修改时间和 run.json
# 放到步骤 2 重建结束之后：run.json 要记 rebuilt、build_exit_code 等只有重建后才知道的字段。
$git = Get-GitInfo
$headCommitTimeIso = ""
if ($git.Available) {
  $headCommitTimeIso = Get-HeadCommitTimeIso
  Write-Log ("git HEAD：" + $git.Head)
  if ($headCommitTimeIso -ne "") { Write-Log ("git HEAD 提交时间：" + $headCommitTimeIso) }
  if ($git.Status.Count -eq 0) { Write-Log "git 工作区：干净" }
  else {
    Write-Log ("git 工作区：" + $git.Status.Count + " 条改动")
    foreach ($s in $git.Status) { Write-Log ("  " + $s) }
  }
} else {
  Write-Log "git 不可用或不在仓库内，run.json 里 git 字段记为空" -Warn
}

$exeExistsBefore = Test-Path -LiteralPath $Exe -PathType Leaf
if ($exeExistsBefore) {
  Write-Log ("步骤 2 前样机已存在：" + $Exe + "（大小、修改时间和 SHA-256 在重建后统一写入 run.json）")
} else {
  Write-Log ("步骤 2 前找不到样机：" + $Exe + "；依赖步骤 2 重建，否则跳过矩阵和内存") -Warn
}

$envChecks = Get-EnvironmentChecks
$envCheckLines = @(Format-EnvironmentChecks -Checks $envChecks)
$envWarnTexts = @($envCheckLines | Where-Object { $_.Warn } | ForEach-Object { $_.Text })
foreach ($l in $envCheckLines) { Write-Log $l.Text -Warn:$l.Warn }

$im = Get-InputMethodInfo
foreach ($err in @($im.Errors)) { Write-Log $err -Warn }
$monitors = Get-MonitorCount
if ($monitors -gt 0) { Write-Log ("显示器：" + $monitors + " 块") }
else { Write-Log "查询显示器数失败，run.json 里记 -1" -Warn }

# 输入法提醒（不阻断）。
if (-not $im.IsEnglishKeyboard) {
  $tipText = $im.DefaultTip
  if ([string]::IsNullOrWhiteSpace($tipText)) { $tipText = "未知" }
  Write-Log ("【警告】默认输入法不是英文键盘（当前默认 TIP：" + $tipText + "）。矩阵搜索首字用当前键盘布局的普通虚拟键触发，中文组合输入可能让首字尚未成为文本（见 tools\focus-matrix\README.md）；建议把「替代默认输入法」设为「中文(简体，中国) - 美式键盘」（TIP 0804:00000409）后重跑。本警告不阻断。") -Warn
}

# Ctrl+C 兜底回调和执行状态。
if (-not ("NianlaiNightGuard" -as [type])) {
  Add-Type -TypeDefinition $script:GuardSource
}
[NianlaiNightGuard]::Enable($script:BatchLogPath, $script:BatchStart)
$script:GuardReady = $true
$esContinuous = [System.Convert]::ToUInt32("80000000", 16)
$esKeepAwake = $esContinuous -bor [System.Convert]::ToUInt32("1", 16) -bor [System.Convert]::ToUInt32("2", 16)
if ([NianlaiNightGuard]::SetExecutionState($esKeepAwake)) {
  $script:ExecutionStateSet = $true
  Write-Log "已设置 SetThreadExecutionState(ES_CONTINUOUS|ES_SYSTEM_REQUIRED|ES_DISPLAY_REQUIRED)：批处理期间不睡眠、显示器保持开"
} else {
  Write-Log "【警告】SetThreadExecutionState 失败，批处理期间系统可能自动睡眠" -Warn
}
Write-Log "步骤 1/9：前置检查完成"

$exitCode = 0
$exeMissingFatal = $false
try {
  # ================= 步骤 2/9：重建样机 =================
  # cmd.exe /d /c：pnpm 是 .cmd 批处理，必须经 cmd 启动；/d 跳过 AutoRun。
  $rebuilt = $false
  $buildExitCode = $null
  if ($SkipBuild) {
    $rebuilt = "skipped"
    Write-Log "按 -SkipBuild 跳过步骤 2（重建样机），直接用当前样机"
    $script:Notes.Add("样机未重建：命令行 -SkipBuild")
  } else {
    $buildExitCode = Invoke-StepCommand -StepName "步骤 2/9：重建样机" -FilePath "cmd.exe" -ArgumentList @("/d", "/c", "pnpm tauri build --no-bundle") -RedirectStdout $buildLog -RedirectStderr $buildStderr -OutputDesc "build-prototype.log"
    if ($buildExitCode -eq 0) {
      $rebuilt = $true
      Write-Log "样机重建成功"
    } else {
      $rebuilt = $false
      Write-Log ("【警告】重建样机失败（退出码 " + $buildExitCode + "，详见 build-prototype.log 和 build-prototype.stderr.txt）") -Warn
    }
  }

  # 步骤 2 后统一确认样机：不存在则矩阵和内存都没法跑，跳过两段直接收尾，退出码 1。
  $exeExists = Test-Path -LiteralPath $Exe -PathType Leaf
  if (-not $exeExists) {
    Write-Log ("【警告】重建后仍找不到样机：" + $Exe + "；跳过矩阵和内存基线，直接收尾，批处理退出码 1") -Warn
    $script:Notes.Add("样机不存在：跳过矩阵（步骤 3–7）和内存基线（步骤 8）")
    $exeMissingFatal = $true
    $exitCode = 1
  } elseif ($rebuilt -is [bool] -and -not $rebuilt) {
    Write-Log ("构建失败但旧样机仍在（" + $Exe + "），沿用旧 exe 继续") -Warn
    $script:Notes.Add("样机未重建，沿用旧 exe")
  }

  # run.json：早上分析结果先看这里；样机元数据取重建后的最终状态。
  $exeSha = ""
  $exeLen = [long]0
  $exeWriteUtc = ""
  $olderThanHead = $false
  if ($exeExists) {
    $exeItem = Get-Item -LiteralPath $Exe
    $exeLen = $exeItem.Length
    $exeWriteUtc = $exeItem.LastWriteTimeUtc.ToString("yyyy-MM-ddTHH:mm:ssZ")
    try { $exeSha = (Get-FileHash -LiteralPath $Exe -Algorithm SHA256).Hash } catch { Write-Log ("计算样机 SHA-256 失败：" + $_.Exception.Message) -Warn }
    Write-Log ("样机：" + $exeLen + " 字节，修改时间 " + $exeItem.LastWriteTime.ToString("yyyy-MM-dd HH:mm:ss") + "，SHA-256 " + $exeSha)
    if ($headCommitTimeIso -ne "") {
      try {
        $headDto = [DateTimeOffset]::Parse($headCommitTimeIso, $script:Invariant, [System.Globalization.DateTimeStyles]::RoundtripKind)
        $olderThanHead = ([DateTimeOffset]$exeItem.LastWriteTime -lt $headDto)
      } catch { Write-Log ("比较样机修改时间和 HEAD 提交时间失败：" + $_.Exception.Message) -Warn }
    }
    if ($olderThanHead) { Write-Log ("【提示】样机修改时间早于 HEAD 提交时间（" + $headCommitTimeIso + "），run.json 里 older_than_head 记 true") -Warn }
  } else {
    Write-Log ("找不到样机：" + $Exe) -Warn
  }
  $runInfo = [ordered]@{
    kind = "g0-night-batch"
    started_at = $script:BatchStart.ToString("yyyy-MM-ddTHH:mm:ss")
    script = $PSCommandPath
    powershell_version = $PSVersionTable.PSVersion.ToString()
    hostname = $env:COMPUTERNAME
    parameters = [ordered]@{
      exe = $Exe
      out = $Out
      matrix_trials = $MatrixTrials
      matrix_max_minutes = $MatrixMaxMinutes
      memory_hours = $MemoryHours.ToString("0.##", $script:Invariant)
      skip_build = [bool]$SkipBuild
      skip_matrix = [bool]$SkipMatrix
    }
    git_head = $git.Head
    git_status_short = @($git.Status)
    prototype = [ordered]@{
      path = $Exe
      exists = [bool]$exeExists
      rebuilt = $rebuilt
      build_exit_code = $buildExitCode
      head_commit_time = $headCommitTimeIso
      older_than_head = [bool]$olderThanHead
      sha256 = $exeSha
      length = $exeLen
      last_write_utc = $exeWriteUtc
    }
    environment_checks = [ordered]@{
      gameviewer_processes = [int]$envChecks.GameViewerCount
      notepad_processes = [int]$envChecks.NotepadCount
      explorer_folder_windows = [int]$envChecks.ExplorerFolderWindows
      screensaver_lock_enabled = [bool]$envChecks.ScreensaverLockEnabled
      warnings = @($envWarnTexts)
    }
    input_method = [ordered]@{
      default_override = $im.OverrideTip
      language_tips = @($im.LanguageTips)
      default_tip = $im.DefaultTip
      looks_english_keyboard = [bool]$im.IsEnglishKeyboard
      note = "默认 TIP 的布局 ID 含 00000409（美式键盘）视为英文键盘；只用于提醒，不参与阻断"
    }
    monitor_count = $monitors
  }
  [System.IO.File]::WriteAllText((Join-Path $Out "run.json"), ($runInfo | ConvertTo-Json -Depth 5), $script:Utf8NoBom)
  Write-Log "已写 run.json（含 rebuilt、build_exit_code、head_commit_time、older_than_head、environment_checks）"

  # ================= 步骤 3–7：矩阵部分 =================
  if ($exeMissingFatal) {
    Write-Log "样机不存在，跳过步骤 3–7（矩阵部分）和步骤 8（内存基线）" -Warn
  } elseif ($SkipMatrix) {
    Write-Log "按 -SkipMatrix 跳过步骤 3–7（矩阵部分），直接做内存基线"
    $script:Notes.Add("跳过矩阵：命令行 -SkipMatrix")
  } else {
    $cargoCmd = Get-Command cargo -ErrorAction SilentlyContinue
    $buildOk = $false
    if ($null -eq $cargoCmd) {
      Write-Log "步骤 3/9：PATH 里没有 cargo，无法构建矩阵工具；跳过步骤 4–6，直接进步骤 7" -Warn
      $script:Notes.Add("跳过矩阵：没有找到 cargo")
    } else {
      $code = Invoke-StepCommand -StepName "步骤 3/9：构建矩阵工具" -FilePath $cargoCmd.Source -ArgumentList @("build", "-q", "-p", "focus-matrix") -OutputDesc "target\debug\focus-matrix.exe"
      $buildOk = ($code -eq 0)
      if (-not $buildOk) {
        Write-Log ("构建失败（退出码 " + $code + "），跳过步骤 4–6，直接进步骤 7") -Warn
        $script:Notes.Add("跳过矩阵：构建 focus-matrix 失败（退出码 " + $code + "）")
      }
    }

    $dryrunOk = $false
    if ($buildOk) {
      $dryRunJson = Join-Path $Out "matrix-dryrun.json"
      $code = Invoke-StepCommand -StepName "步骤 4/9：矩阵空跑" -FilePath $matrixExe -ArgumentList @("--dry-run", "--json", "--exe", $Exe) -RedirectStdout $dryRunJson -RedirectStderr (Join-Path $Out "matrix-dryrun.stderr.txt") -OutputDesc "matrix-dryrun.json"
      $dryrunOk = ($code -eq 0)
      # 空跑摘要：blockers、时长估计和每个场景的 id/status 抄进日志和汇总；解析失败只记一句，不影响流程。
      $script:MatrixDryrunLines.Clear()
      try {
        $dryObj = Get-Content -LiteralPath $dryRunJson -Raw -Encoding UTF8 | ConvertFrom-Json
        foreach ($b in @($dryObj.blockers)) { if ($b) { $script:MatrixDryrunLines.Add("阻断：" + $b) } }
        if ($null -ne $dryObj.estimated_minutes -or $null -ne $dryObj.upper_minutes) {
          $script:MatrixDryrunLines.Add("预计 " + [string]$dryObj.estimated_minutes + " 分钟，上界 " + [string]$dryObj.upper_minutes + " 分钟")
        }
        foreach ($p in @($dryObj.plan)) { if ($p) { $script:MatrixDryrunLines.Add("场景 " + $p.id + "：" + $p.status) } }
      } catch {
        Write-Log ("解析 matrix-dryrun.json 失败：" + $_.Exception.Message) -Warn
      }
      foreach ($l in $script:MatrixDryrunLines) { Write-Log ("空跑摘要：" + $l) }
      if (-not $dryrunOk) {
        Write-Log ("空跑退出码 " + $code + "（空跑发现阻断，详见 matrix-dryrun.json），跳过步骤 5–6，直接进步骤 7") -Warn
        $script:Notes.Add("跳过冒烟和全量：矩阵空跑退出码 " + $code)
      }
    }

    $smokeOk = $false
    if ($dryrunOk) {
      $code = Invoke-StepCommand -StepName "步骤 5/9：矩阵冒烟" -FilePath $matrixExe -ArgumentList @("--real-input", "--only", "helper", "--trials", "4", "--exe", $Exe, "--out", $matrixSmokeDir) -RedirectStdout $matrixSmokeJson -RedirectStderr $matrixSmokeProgress -OutputDesc "matrix-smoke.json、matrix-smoke\"
      $smokeOk = ($code -eq 0)
      if (-not $smokeOk) {
        Write-Log ("冒烟退出码 " + $code + "（详见 matrix-smoke.json 和 matrix-smoke\ 目录），跳过步骤 6，直接进步骤 7") -Warn
        $script:Notes.Add("跳过全量：冒烟退出码 " + $code)
      }
    }

    if ($smokeOk) {
      $code = Invoke-StepCommand -StepName "步骤 6/9：矩阵全量" -FilePath $matrixExe -ArgumentList @("--real-input", "--trials", [string]$MatrixTrials, "--max-minutes", [string]$MatrixMaxMinutes, "--exe", $Exe, "--out", $matrixFullDir) -RedirectStdout $matrixFullJson -RedirectStderr $matrixFullProgress -OutputDesc "matrix-full.json、matrix-full\"
      if ($code -ne 0) {
        Write-Log ("矩阵全量退出码 " + $code + "（阻断、失败、中止或有效次数不足，详见 matrix-full.json 和 matrix-full\ 目录）；按计划继续步骤 7") -Warn
        $script:Notes.Add("矩阵全量退出码 " + $code + "，详见 matrix-full\summary.md")
      }
    }

    # ================= 步骤 7/9：清理和等待 =================
    $sw7 = [System.Diagnostics.Stopwatch]::StartNew()
    Write-Log "步骤 7/9：清理和等待：开始（最多等 120 秒直到没有 nianlai.exe）"
    $deadline = (Get-Date).AddSeconds(120)
    while ($true) {
      $left = @(Get-Process -Name "nianlai" -ErrorAction SilentlyContinue)
      if ($left.Count -eq 0) { break }
      if ((Get-Date) -ge $deadline) {
        Write-Log ("等待 120 秒后仍有 " + $left.Count + " 个 nianlai.exe，转入按 PID 清理") -Warn
        break
      }
      Start-Sleep -Seconds 5
    }
    $leftover = @(Get-Process -Name "nianlai" -ErrorAction SilentlyContinue)
    if ($leftover.Count -eq 0) {
      Write-Log "没有 nianlai.exe 在运行，无需清理"
    }
    foreach ($p in $leftover) {
      $started = $null
      try { $started = $p.StartTime } catch { }
      if ($null -eq $started) {
        Write-Log ("PID " + $p.Id + "：拿不到启动时间，为避免误杀不动它") -Warn
        $script:Notes.Add("步骤 7：PID " + $p.Id + " 拿不到启动时间，未结束")
        continue
      }
      if ($started -le $script:BatchStart) {
        Write-Log ("PID " + $p.Id + "：启动于 " + $started.ToString("yyyy-MM-dd HH:mm:ss") + "，不晚于批处理开始，不动它") -Warn
        $script:Notes.Add("步骤 7：PID " + $p.Id + " 启动时间早于批处理开始，未结束")
        continue
      }
      Write-Log ("PID " + $p.Id + "：启动于 " + $started.ToString("yyyy-MM-dd HH:mm:ss") + "，晚于批处理开始，结束它")
      try {
        Stop-Process -Id $p.Id -Force -ErrorAction Stop
        Write-Log ("PID " + $p.Id + "：已结束")
      } catch {
        Write-Log ("PID " + $p.Id + "：结束失败：" + $_.Exception.Message) -Warn
      }
    }
    $sw7.Stop()
    $finalCount = @(Get-Process -Name "nianlai" -ErrorAction SilentlyContinue).Count
    Write-Log ("步骤 7/9：结束，剩余 nianlai.exe " + $finalCount + " 个，耗时 " + (Format-DurationText $sw7.Elapsed.TotalSeconds))
    $script:Steps.Add([pscustomobject]@{
      name = "步骤 7/9：清理和等待"
      command = "等待最多 120 秒；仍有的 nianlai.exe 只结束启动时间晚于批处理开始的（Stop-Process -Id）"
      exit_code = 0
      seconds = [math]::Round($sw7.Elapsed.TotalSeconds, 1)
      output = ""
    })
    if ($finalCount -gt 0) {
      $script:Notes.Add("步骤 7 结束时仍有 " + $finalCount + " 个 nianlai.exe；内存实验若因它拒绝启动，退出码见步骤 8")
    }
  }

  # ================= 步骤 8/9：内存基线 =================
  # 矩阵阶段已结束（或被跳过）：不再强制显示器常亮，和以后单独跑的重挂起实验条件一致。
  $esSystemAwake = $esContinuous -bor [System.Convert]::ToUInt32("1", 16)
  if (-not [NianlaiNightGuard]::SetExecutionState($esSystemAwake)) {
    Write-Log "【警告】下调执行状态失败，显示器仍会保持常亮（ES_CONTINUOUS|ES_SYSTEM_REQUIRED|ES_DISPLAY_REQUIRED）" -Warn
  } else {
    Write-Log "矩阵阶段已结束，显示器按电源计划处理，系统仍不睡眠（已改为 ES_CONTINUOUS|ES_SYSTEM_REQUIRED）"
  }
  if (-not $exeMissingFatal) {
    $memHoursText = $MemoryHours.ToString("0.##", $script:Invariant)
    $memArgs = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $memoryScript, "-Exe", $Exe, "-ResuspendMinutes", "0", "-DurationHours", $memHoursText, "-Label", "baseline", "-OutDir", $memoryOutDir)
    $code = Invoke-StepCommand -StepName "步骤 8/9：内存基线" -FilePath "powershell.exe" -ArgumentList $memArgs -OutputDesc "memory-baseline\"
    if ($code -ne 0) {
      Write-Log ("内存基线退出码 " + $code + "（它会在已有 nianlai.exe、找不到样机或异常中断时非 0，详见 memory-baseline\ 目录）") -Warn
      $script:Notes.Add("内存基线退出码 " + $code)
    }
  }

  # ================= 步骤 9/9：收尾 =================
  $summaryPath = Join-Path $Out "summary.txt"
  $lines = New-Object System.Collections.Generic.List[string]
  $endedAt = Get-Date
  $lines.Add("拈来 G0 夜间批处理汇总")
  $lines.Add("开始：" + $script:BatchStart.ToString("yyyy-MM-dd HH:mm:ss"))
  $lines.Add("结束：" + $endedAt.ToString("yyyy-MM-dd HH:mm:ss"))
  $lines.Add("总耗时：" + (Format-DurationText ($endedAt - $script:BatchStart).TotalSeconds))
  $lines.Add("仓库：" + $repoRoot)
  $lines.Add("样机：" + $Exe)
  $lines.Add("git HEAD：" + $git.Head)
  $lines.Add("")
  $lines.Add("—— 步骤结果 ——")
  foreach ($s in $script:Steps) {
    $lines.Add($s.name + "：退出码 " + $s.exit_code + "，耗时 " + (Format-DurationText ([double]$s.seconds)))
    $lines.Add("  命令：" + $s.command)
    if ($s.output -ne "") { $lines.Add("  产出：" + $s.output) }
  }
  if ($script:Notes.Count -gt 0) {
    $lines.Add("")
    $lines.Add("—— 跳过与说明 ——")
    foreach ($n in $script:Notes) { $lines.Add("- " + $n) }
  }
  if ($script:MatrixDryrunLines.Count -gt 0) {
    $lines.Add("")
    $lines.Add("—— 矩阵空跑摘要（matrix-dryrun.json）——")
    foreach ($l in $script:MatrixDryrunLines) { $lines.Add($l) }
  }
  $matrixMd = Join-Path $matrixFullDir "summary.md"
  $lines.Add("")
  $lines.Add("—— 矩阵全量 summary.md（" + $matrixMd + "）——")
  if (Test-Path -LiteralPath $matrixMd) {
    $lines.AddRange([string[]](Get-Content -LiteralPath $matrixMd -Encoding UTF8))
  } else {
    $lines.Add("（未生成：全量没有跑到或被跳过）")
  }
  $memSummary = Join-Path $memoryOutDir "summary.txt"
  $lines.Add("")
  $lines.Add("—— 内存基线 summary.txt 前 30 行（" + $memSummary + "）——")
  if (Test-Path -LiteralPath $memSummary) {
    $lines.AddRange([string[]](Get-Content -LiteralPath $memSummary -Encoding UTF8 -TotalCount 30))
  } else {
    $lines.Add("（未生成：内存实验没有正常结束）")
  }
  [System.IO.File]::WriteAllText($summaryPath, (($lines -join "`r`n") + "`r`n"), $script:Utf8NoBom)
  $lines | ForEach-Object { Write-Host $_ }
  Write-Log ("已写 summary.txt：" + $summaryPath)
  Write-Log "批处理完成"
} catch {
  Write-Log ("批处理异常终止：" + $_.Exception.Message) -Warn
  $exitCode = 1
} finally {
  if ($script:ExecutionStateSet) {
    [void][NianlaiNightGuard]::SetExecutionState($esContinuous)
    $script:ExecutionStateSet = $false
    Write-Log "已恢复系统睡眠设置（SetThreadExecutionState(ES_CONTINUOUS)）"
  }
}
exit $exitCode
