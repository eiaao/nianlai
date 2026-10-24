<#
.SYNOPSIS
  测内存紧张时的呼出耗时（G0）：WebView2 挂起并压缩之后，页面被换出时再呼出要多久。

.DESCRIPTION
  启动正式版样机（独立的临时数据目录，自动显示开关 NIANLAI_G0_AUTOCYCLE），让它反复“隐藏足够久、显示、再隐藏”，
  从样机日志的 shown、painted 事件里取每次显示的耗时。三组条件：
    none      不施压，对照；
    trim      显示前几秒对样机进程树（nianlai.exe 和它的 WebView2 子进程）调 EmptyWorkingSet，模拟页面被修剪出工作集；
    pressure  另起一个辅助进程占住“可用物理内存减去安全余量”，让系统处在内存紧张的状态，再看呼出耗时。
  -Mode trim（默认）测 none 和 trim，两组交替进行；-Mode pressure 在此之后再测一段 pressure。
  每次显示前后各采一次：系统可用内存、空闲页和备用页、系统累计的页面读取次数（硬缺页的旁证）、样机进程树的缺页数。
  结果写进输出目录：shows.csv（每次显示一行）、summary.txt（中文摘要）、run.json、panel-log.jsonl（本次样机日志），
  pressure 模式另有 helper-status.jsonl（辅助进程的分配记录）。
  只按 PID 结束本脚本自己启动的进程；默认拒绝在已有 nianlai.exe 运行时启动。
  运行期间阻止系统睡眠并让显示器保持开着（显示器关掉或锁屏后页面不按时绘制，计时会失真），-AllowSleep 可关闭这一行为，但不建议。

.PARAMETER Mode
  trim（默认）或 pressure。

.PARAMETER DryRun
  不启动任何进程、不施压，只检查参数、样机路径、可用内存，并打印计划。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\measure-show-under-pressure.ps1 -DryRun
  只看计划：每组几次、预计多久、pressure 要占多少内存。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\measure-show-under-pressure.ps1
  默认：none 和 trim 各 30 次，交替进行，约 45 分钟。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\measure-show-under-pressure.ps1 -Mode pressure -Cycles 30
  三组各 30 次，pressure 放在最后，约 70 分钟。
#>
[CmdletBinding()]
param(
  [ValidateSet("trim", "pressure")]
  [string]$Mode = "trim",
  [string]$Exe = "",
  [int]$Cycles = 30,
  [int]$WarmupCycles = 2,
  [double]$HiddenSeconds = 40,
  [double]$VisibleSeconds = 3,
  [double]$TrimLeadSeconds = 3,
  [int]$MarginMB = 2048,
  [int]$CommitMarginMB = 4096,
  [int]$MaxPressureMB = 0,
  [int]$TopUpCapMB = 2048,
  [double]$TopUpAfterSeconds = 27,
  [int]$RampSeconds = 120,
  [int]$PressureMaxMinutes = 60,
  [string]$Hotkey = "Ctrl+Shift+F11",
  [string]$Label = "",
  [string]$OutDir = "",
  [switch]$AllowExisting,
  [switch]$AllowSleep,
  [switch]$DryRun,
  # 以下参数只给本脚本以辅助进程身份启动自己时使用，手工运行不要设置。
  [switch]$PressureHelper,
  [int]$HelperParentPid = 0,
  [string]$HelperDir = "",
  [long]$HelperDeadlineMs = 0
)

try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$script:Invariant = [System.Globalization.CultureInfo]::InvariantCulture

# 安全余量的下限和其他硬性范围；改这些数字之前先想清楚会不会把机器压到卡死。
$script:MinMarginMB = 1024
$script:ChunkMB = 256
$script:MaxPressureMinutesLimit = 180

function Get-NowMs { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }

function Format-Num([double]$Value, [string]$Pattern = "0.0") {
  $Value.ToString($Pattern, $script:Invariant)
}

# ---------- 原生接口（C# 5 语法，PowerShell 5.1 的编译器只认这个版本） ----------
$script:NativeSource = @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public static class NianlaiShowNative {
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  struct PROCESSENTRY32W {
    public uint dwSize; public uint cntUsage; public uint th32ProcessID; public IntPtr th32DefaultHeapID;
    public uint th32ModuleID; public uint cntThreads; public uint th32ParentProcessID; public int pcPriClassBase;
    public uint dwFlags;
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string szExeFile;
  }

  [StructLayout(LayoutKind.Sequential)]
  struct PROCESS_MEMORY_COUNTERS_EX2 {
    public uint cb; public uint PageFaultCount; public UIntPtr PeakWorkingSetSize; public UIntPtr WorkingSetSize;
    public UIntPtr QuotaPeakPagedPoolUsage; public UIntPtr QuotaPagedPoolUsage; public UIntPtr QuotaPeakNonPagedPoolUsage;
    public UIntPtr QuotaNonPagedPoolUsage; public UIntPtr PagefileUsage; public UIntPtr PeakPagefileUsage;
    public UIntPtr PrivateUsage; public UIntPtr PrivateWorkingSetSize; public ulong SharedCommitUsage;
  }

  [StructLayout(LayoutKind.Sequential)]
  struct MEMORY_PRIORITY_INFORMATION { public uint MemoryPriority; }

  [StructLayout(LayoutKind.Sequential)]
  class MEMORYSTATUSEX {
    public uint dwLength = (uint)Marshal.SizeOf(typeof(MEMORYSTATUSEX));
    public uint dwMemoryLoad; public ulong ullTotalPhys; public ulong ullAvailPhys; public ulong ullTotalPageFile;
    public ulong ullAvailPageFile; public ulong ullTotalVirtual; public ulong ullAvailVirtual; public ulong ullAvailExtendedVirtual;
  }

  const uint TH32CS_SNAPPROCESS = 0x2;
  const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
  const uint PROCESS_SET_QUOTA = 0x0100;
  const uint PROCESS_QUERY_INFORMATION = 0x0400;
  const uint MEM_COMMIT_RESERVE = 0x3000;
  const uint MEM_RELEASE = 0x8000;
  const uint PAGE_READWRITE = 0x04;

  [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
  [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool Process32FirstW(IntPtr snapshot, ref PROCESSENTRY32W entry);
  [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool Process32NextW(IntPtr snapshot, ref PROCESSENTRY32W entry);
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool CloseHandle(IntPtr handle);
  [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool K32GetProcessMemoryInfo(IntPtr process, ref PROCESS_MEMORY_COUNTERS_EX2 counters, uint size);
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool K32EmptyWorkingSet(IntPtr process);
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetProcessInformation(IntPtr process, int infoClass, ref MEMORY_PRIORITY_INFORMATION info, uint size);
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool GlobalMemoryStatusEx([In, Out] MEMORYSTATUSEX status);
  [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr VirtualAlloc(IntPtr address, UIntPtr size, uint type, uint protect);
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool VirtualFree(IntPtr address, UIntPtr size, uint type);
  [DllImport("kernel32.dll")] static extern uint SetThreadExecutionState(uint flags);

  // 运行期间阻止系统睡眠，并让显示器保持开着：显示器关掉后页面不再按时绘制，首帧计时会失真。关掉时恢复。
  public static void KeepAwake(bool on) {
    SetThreadExecutionState(on ? 0x80000003u : 0x80000000u);
  }

  // 从 root 出发按父子关系收集整棵进程树；visited 防止 PID 复用成环。names 与返回的 PID 一一对应。
  public static uint[] Tree(uint root, out string[] names) {
    var parentOf = new Dictionary<uint, uint>();
    var nameOf = new Dictionary<uint, string>();
    var children = new Dictionary<uint, List<uint>>();
    IntPtr snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (snapshot != IntPtr.Zero && snapshot != new IntPtr(-1)) {
      var entry = new PROCESSENTRY32W();
      entry.dwSize = (uint)Marshal.SizeOf(typeof(PROCESSENTRY32W));
      if (Process32FirstW(snapshot, ref entry)) {
        do {
          parentOf[entry.th32ProcessID] = entry.th32ParentProcessID;
          nameOf[entry.th32ProcessID] = entry.szExeFile;
          List<uint> list;
          if (!children.TryGetValue(entry.th32ParentProcessID, out list)) { list = new List<uint>(); children[entry.th32ParentProcessID] = list; }
          list.Add(entry.th32ProcessID);
        } while (Process32NextW(snapshot, ref entry));
      }
      CloseHandle(snapshot);
    }
    var result = new List<uint>();
    var resultNames = new List<string>();
    if (nameOf.ContainsKey(root)) {
      var visited = new HashSet<uint>();
      var queue = new Queue<uint>();
      visited.Add(root); queue.Enqueue(root);
      while (queue.Count > 0) {
        uint pid = queue.Dequeue();
        result.Add(pid); resultNames.Add(nameOf[pid]);
        List<uint> list;
        if (children.TryGetValue(pid, out list)) {
          foreach (uint child in list) { if (child != pid && visited.Add(child)) queue.Enqueue(child); }
        }
      }
    }
    names = resultNames.ToArray();
    return result.ToArray();
  }

  // 返回 [专用工作集字节数合计, 缺页次数合计, 读到的进程数, 私有提交字节数合计]。读不到的进程不计入。
  public static long[] Sample(uint[] pids) {
    long privateWs = 0, faults = 0, readable = 0, commit = 0;
    foreach (uint pid in pids) {
      IntPtr process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
      if (process == IntPtr.Zero) continue;
      var counters = new PROCESS_MEMORY_COUNTERS_EX2();
      counters.cb = (uint)Marshal.SizeOf(typeof(PROCESS_MEMORY_COUNTERS_EX2));
      if (K32GetProcessMemoryInfo(process, ref counters, counters.cb)) {
        privateWs += (long)counters.PrivateWorkingSetSize.ToUInt64();
        faults += counters.PageFaultCount;
        commit += (long)counters.PrivateUsage.ToUInt64();
        readable++;
      }
      CloseHandle(process);
    }
    return new long[] { privateWs, faults, readable, commit };
  }

  // 修剪单个进程的工作集，成功返回 0，失败返回 Win32 错误码（5 表示拒绝访问）。
  public static int Trim(uint pid) {
    IntPtr process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_QUOTA, false, pid);
    if (process == IntPtr.Zero) return Marshal.GetLastWin32Error();
    int error = 0;
    if (!K32EmptyWorkingSet(process)) error = Marshal.GetLastWin32Error();
    CloseHandle(process);
    return error;
  }

  // 进程的内存优先级（1 最低，5 正常）；先用受限查询权限，不行再用完整查询权限；都读不到返回 -1。
  public static int MemoryPriority(uint pid) {
    foreach (uint access in new uint[] { PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_QUERY_INFORMATION }) {
      IntPtr process = OpenProcess(access, false, pid);
      if (process == IntPtr.Zero) continue;
      var info = new MEMORY_PRIORITY_INFORMATION();
      bool ok = GetProcessInformation(process, 0, ref info, (uint)Marshal.SizeOf(typeof(MEMORY_PRIORITY_INFORMATION)));
      CloseHandle(process);
      if (ok) return (int)info.MemoryPriority;
    }
    return -1;
  }

  // 返回 [可用物理内存, 物理内存总量, 可用提交量, 提交上限]，单位字节。
  public static ulong[] MemoryStatus() {
    var status = new MEMORYSTATUSEX();
    if (!GlobalMemoryStatusEx(status)) return new ulong[] { 0, 0, 0, 0 };
    return new ulong[] { status.ullAvailPhys, status.ullTotalPhys, status.ullAvailPageFile, status.ullTotalPageFile };
  }

  public static IntPtr Allocate(long bytes) {
    return VirtualAlloc(IntPtr.Zero, new UIntPtr((ulong)bytes), MEM_COMMIT_RESERVE, PAGE_READWRITE);
  }

  // 每页写一个字节，让系统真的分配物理页。
  public static void Touch(IntPtr address, long bytes) {
    long start = address.ToInt64();
    for (long offset = 0; offset < bytes; offset += 4096) Marshal.WriteByte(new IntPtr(start + offset), (byte)0x5A);
  }

  public static bool Release(IntPtr address) {
    return VirtualFree(address, UIntPtr.Zero, MEM_RELEASE);
  }
}
'@

function Initialize-Native {
  if (-not ("NianlaiShowNative" -as [type])) { Add-Type -TypeDefinition $script:NativeSource -Language CSharp }
}

# ---------- 辅助进程：占内存（本脚本以 -PressureHelper 启动自己时走这里） ----------
# 内存属于这个进程，它一退出（包括被强制结束）系统就会全部收回，所以主脚本出任何问题时只要按 PID 结束它即可。
# 自己也会在这些情况下释放并退出：主脚本进程不在了、到了截止时间、出现停止文件、出错。
if ($PressureHelper) {
  $statusPath = Join-Path $HelperDir "helper-status.jsonl"
  $stopPath = Join-Path $HelperDir "helper-stop"
  $controlPath = Join-Path $HelperDir "helper-control.txt"
  $blocks = New-Object System.Collections.Generic.List[object]
  $script:AllocatedMB = 0
  $script:TopUpMB = 0
  $exitReason = "unknown"

  function Write-HelperStatus([string]$State, [hashtable]$Extra) {
    $row = [ordered]@{ time_ms = (Get-NowMs); state = $State; allocated_mb = $script:AllocatedMB; topup_mb = $script:TopUpMB }
    try {
      $status = [NianlaiShowNative]::MemoryStatus()
      $row["avail_mb"] = [math]::Round($status[0] / 1MB, 0)
      $row["commit_avail_mb"] = [math]::Round($status[2] / 1MB, 0)
    } catch { }
    if ($null -ne $Extra) { foreach ($key in $Extra.Keys) { $row[$key] = $Extra[$key] } }
    try { [System.IO.File]::AppendAllText($statusPath, (($row | ConvertTo-Json -Compress) + "`n"), $script:Utf8NoBom) } catch { }
  }

  function Get-HelperStopReason($Config) {
    if (Test-Path -LiteralPath $stopPath) { return "stop_file" }
    if ((Get-NowMs) -ge $HelperDeadlineMs) { return "deadline" }
    $parent = Get-Process -Id $HelperParentPid -ErrorAction SilentlyContinue
    if ($null -eq $parent) { return "parent_gone" }
    try { if ([string]$parent.StartTime.ToUniversalTime().Ticks -ne [string]$Config.parent_start_ticks) { return "parent_gone" } } catch { }
    return $null
  }

  # 按块分配并逐页写入，每块之前都重新检查余量；返回实际分配的 MB。
  function Add-Blocks([int]$WantMB, [string]$State, $Config) {
    $done = 0
    while ($done -lt $WantMB) {
      $reason = Get-HelperStopReason $Config
      if ($null -ne $reason) { throw "stop:$reason" }
      $chunk = [math]::Min($script:ChunkMB, $WantMB - $done)
      $status = [NianlaiShowNative]::MemoryStatus()
      $availMB = $status[0] / 1MB
      $commitMB = $status[2] / 1MB
      if ($availMB - $chunk -lt [double]$Config.margin_mb) { Write-HelperStatus ($State + "_stopped") @{ reason = "margin" }; break }
      if ($commitMB - $chunk -lt [double]$Config.commit_margin_mb) { Write-HelperStatus ($State + "_stopped") @{ reason = "commit" }; break }
      $address = [NianlaiShowNative]::Allocate([long]$chunk * 1MB)
      if ($address -eq [IntPtr]::Zero) { Write-HelperStatus ($State + "_stopped") @{ reason = "allocate_failed" }; break }
      $blocks.Add([pscustomobject]@{ Address = $address; MB = $chunk })
      $script:AllocatedMB += $chunk
      [NianlaiShowNative]::Touch($address, [long]$chunk * 1MB)
      $done += $chunk
      Write-HelperStatus $State $null
      Start-Sleep -Milliseconds 20
    }
    return $done
  }

  function Remove-LastBlock {
    if ($blocks.Count -eq 0) { return $false }
    $last = $blocks[$blocks.Count - 1]
    [void][NianlaiShowNative]::Release($last.Address)
    $blocks.RemoveAt($blocks.Count - 1)
    $script:AllocatedMB -= $last.MB
    return $true
  }

  try {
    Initialize-Native
    try { (Get-Process -Id $PID).PriorityClass = [System.Diagnostics.ProcessPriorityClass]::BelowNormal } catch { }
    $config = [System.IO.File]::ReadAllText((Join-Path $HelperDir "helper-config.json"), $script:Utf8NoBom) | ConvertFrom-Json
    $floorMB = [double]$config.floor_mb
    $status = [NianlaiShowNative]::MemoryStatus()
    $targetMB = [math]::Floor([math]::Min($status[0] / 1MB - [double]$config.margin_mb, $status[2] / 1MB - [double]$config.commit_margin_mb))
    if ([int]$config.max_pressure_mb -gt 0) { $targetMB = [math]::Min($targetMB, [double]$config.max_pressure_mb) }
    Write-HelperStatus "ramp_start" @{ target_mb = $targetMB }
    if ($targetMB -ge $script:ChunkMB) { [void](Add-Blocks ([int]$targetMB) "ramp" $config) }
    Write-HelperStatus "ready" @{ target_mb = $targetMB }
    $handled = 0
    while ($true) {
      $reason = Get-HelperStopReason $config
      if ($null -ne $reason) { $exitReason = $reason; break }
      # 别的程序占用增长、可用内存掉到下限以下时，立刻按块释放，直到回到安全余量。
      $status = [NianlaiShowNative]::MemoryStatus()
      if ($status[0] / 1MB -lt $floorMB -and $blocks.Count -gt 0) {
        $released = 0
        while ($blocks.Count -gt 0 -and ([NianlaiShowNative]::MemoryStatus())[0] / 1MB -lt [double]$config.margin_mb) {
          $released += $blocks[$blocks.Count - 1].MB
          [void](Remove-LastBlock)
        }
        Write-HelperStatus "emergency_release" @{ released_mb = $released }
      }
      # 主脚本每次在压缩之后写一个更大的序号，请求“补压”：把可用内存重新占到安全余量，累计不超过上限。
      $requested = 0
      if (Test-Path -LiteralPath $controlPath) {
        # 主脚本可能正在写这个文件，读失败就等下一轮，不能因此退出。
        try { [void][int]::TryParse([System.IO.File]::ReadAllText($controlPath).Trim(), [ref]$requested) } catch { $requested = $handled }
      }
      while ($handled -lt $requested) {
        $handled++
        $status = [NianlaiShowNative]::MemoryStatus()
        $needMB = [math]::Floor($status[0] / 1MB - [double]$config.margin_mb)
        $roomMB = [int]$config.topup_cap_mb - $script:TopUpMB
        $wantMB = [int][math]::Max(0, [math]::Min($needMB, $roomMB))
        $gotMB = 0
        if ($wantMB -ge 16) { $gotMB = Add-Blocks $wantMB "topup_alloc" $config }
        $script:TopUpMB += $gotMB
        Write-HelperStatus "topup" @{ request = $handled; want_mb = $wantMB; got_mb = $gotMB }
      }
      Start-Sleep -Milliseconds 200
    }
  } catch {
    $message = $_.Exception.Message
    if ($message -like "stop:*") { $exitReason = $message.Substring(5) } else { $exitReason = "error: $message" }
  } finally {
    while (Remove-LastBlock) { }
    Write-HelperStatus "released" @{ reason = $exitReason }
  }
  exit 0
}

# ---------- 主脚本：检查参数 ----------
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($Exe)) { $Exe = Join-Path $repoRoot "target\release\nianlai.exe" }
if ([string]::IsNullOrWhiteSpace($Label)) { $Label = $Mode }
if ([string]::IsNullOrWhiteSpace($OutDir)) {
  $OutDir = Join-Path $repoRoot ("target\g0-show-pressure\{0:yyyyMMdd-HHmmss}-{1}" -f (Get-Date), $Label)
}
# 辅助进程要用同一个目录，先换成不带末尾反斜杠的绝对路径。
$OutDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutDir).TrimEnd([char]92)
$usePressure = ($Mode -eq "pressure")

$problems = New-Object System.Collections.Generic.List[string]
$warnings = New-Object System.Collections.Generic.List[string]
if ($Cycles -lt 1) { $problems.Add("-Cycles 至少为 1") }
if ($WarmupCycles -lt 0 -or $WarmupCycles -gt 10) { $problems.Add("-WarmupCycles 要在 0 到 10 之间") }
# 挂起后约 23 秒才压缩（启动后的第一次要 30–32 秒，由预热轮吸收），隐藏时间短于 30 秒就测不到压缩后的状态。
if ($HiddenSeconds -lt 30 -or $HiddenSeconds -gt 600) { $problems.Add("-HiddenSeconds 要在 30 到 600 之间（挂起后约 23 秒才压缩）") }
if ($VisibleSeconds -lt 1 -or $VisibleSeconds -gt 30) { $problems.Add("-VisibleSeconds 要在 1 到 30 之间") }
# 修剪要在压缩之后（隐藏约 24 秒）再留 2 秒，也不能和显示挤在一起。
# 显示前 1.5 秒还要采样一次，修剪必须在它之前。
if ($TrimLeadSeconds -lt 2 -or $TrimLeadSeconds -gt ($HiddenSeconds - 26)) { $problems.Add("-TrimLeadSeconds 要在 2 到（隐藏秒数 - 26）之间，保证在压缩之后、显示前的采样之前修剪") }
if ($MarginMB -lt $script:MinMarginMB) { $problems.Add("-MarginMB 不能小于 $($script:MinMarginMB)，否则可能把机器压到卡死") }
if ($CommitMarginMB -lt 1024) { $problems.Add("-CommitMarginMB 不能小于 1024，否则别的程序可能申请不到内存") }
if ($MaxPressureMB -lt 0) { $problems.Add("-MaxPressureMB 不能为负；0 表示不另设上限") }
if ($TopUpCapMB -lt 0 -or $TopUpCapMB -gt 8192) { $problems.Add("-TopUpCapMB 要在 0 到 8192 之间") }
if ($TopUpAfterSeconds -lt 25 -or $TopUpAfterSeconds -gt ($HiddenSeconds - 5)) { $problems.Add("-TopUpAfterSeconds 要在 25 到（隐藏秒数 - 5）之间，保证在压缩之后、显示之前补压") }
if ($RampSeconds -lt 30 -or $RampSeconds -gt 600) { $problems.Add("-RampSeconds 要在 30 到 600 之间") }
if ($PressureMaxMinutes -lt 5 -or $PressureMaxMinutes -gt $script:MaxPressureMinutesLimit) { $problems.Add("-PressureMaxMinutes 要在 5 到 $($script:MaxPressureMinutesLimit) 之间") }
if ([string]::IsNullOrWhiteSpace($Hotkey)) { $problems.Add("-Hotkey 不能为空") }
if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) { $problems.Add("找不到样机：$Exe（先在项目根目录运行 pnpm tauri build --no-bundle）") }

# ---------- 计划：每一步“隐藏秒数:显示秒数”，和每一步属于哪组条件 ----------
$steps = New-Object System.Collections.Generic.List[object]
for ($i = 0; $i -lt $WarmupCycles; $i++) { $steps.Add([pscustomobject]@{ Condition = "warmup"; Hidden = $HiddenSeconds }) }
# none 和 trim 交替，两组经历同样的时间段，减少后台活动随时间变化带来的偏差。
for ($i = 0; $i -lt $Cycles; $i++) {
  $steps.Add([pscustomobject]@{ Condition = "none"; Hidden = $HiddenSeconds })
  $steps.Add([pscustomobject]@{ Condition = "trim"; Hidden = $HiddenSeconds })
}
if ($usePressure) {
  # 第一个 pressure 步的隐藏时间加上 RampSeconds，留给辅助进程占内存。
  for ($i = 0; $i -lt $Cycles; $i++) {
    $hidden = $HiddenSeconds
    if ($i -eq 0) { $hidden = $HiddenSeconds + $RampSeconds }
    $steps.Add([pscustomobject]@{ Condition = "pressure"; Hidden = $hidden })
  }
}
if ($steps.Count -gt 1000) { $problems.Add("总步数 $($steps.Count) 超过样机允许的 1000 步，请减少 -Cycles") }
$planText = ($steps | ForEach-Object { "{0}:{1}" -f (Format-Num $_.Hidden "0.###"), (Format-Num $VisibleSeconds "0.###") }) -join ","

# 每步实际耗时 ≈ 隐藏 + 显示 + 挂起回执（约 0.5 秒，按上限估）。
$stepOverhead = 0.5
$plannedSeconds = 0.0
$pressureSeconds = 0.0
foreach ($step in $steps) {
  $plannedSeconds += $step.Hidden + $VisibleSeconds + $stepOverhead
  if ($step.Condition -eq "pressure") { $pressureSeconds += $step.Hidden + $VisibleSeconds + $stepOverhead }
}
$startupSeconds = 15
$shutdownSeconds = 30
$totalSeconds = $plannedSeconds + $startupSeconds + $shutdownSeconds
if ($usePressure -and ($pressureSeconds / 60.0 + 5) -gt $PressureMaxMinutes) {
  $problems.Add(("pressure 阶段计划 {0} 分钟（另加 5 分钟余量），超过 -PressureMaxMinutes {1}；请减少 -Cycles 或调大上限（最多 {2}）" -f (Format-Num ($pressureSeconds / 60.0) "0.0"), $PressureMaxMinutes, $script:MaxPressureMinutesLimit))
}

# ---------- 当前内存（只读查询，不启动进程） ----------
$memory = $null
try {
  $os = Get-CimInstance Win32_OperatingSystem -Property TotalVisibleMemorySize, FreePhysicalMemory, TotalVirtualMemorySize, FreeVirtualMemory -ErrorAction Stop
  $memory = [ordered]@{
    total_mb = [math]::Round([double]$os.TotalVisibleMemorySize / 1024, 0)
    avail_mb = [math]::Round([double]$os.FreePhysicalMemory / 1024, 0)
    commit_limit_mb = [math]::Round([double]$os.TotalVirtualMemorySize / 1024, 0)
    commit_avail_mb = [math]::Round([double]$os.FreeVirtualMemory / 1024, 0)
  }
} catch { $warnings.Add("读不到系统内存：$($_.Exception.Message)") }

function Get-MemoryLists {
  # 原始计数器：页面读取次数是累计值（硬缺页的旁证），各页面列表是当前字节数。读不到的项为空。
  try {
    $m = Get-CimInstance Win32_PerfRawData_PerfOS_Memory -ErrorAction Stop
    return [ordered]@{
      page_reads = $m.PageReadsPersec
      pages_input = $m.PagesInputPersec
      free_zero_mb = $(if ($null -ne $m.FreeAndZeroPageListBytes) { [math]::Round([double]$m.FreeAndZeroPageListBytes / 1MB, 0) } else { $null })
      standby_mb = $(if ($null -ne $m.StandbyCacheNormalPriorityBytes) { [math]::Round(([double]$m.StandbyCacheCoreBytes + [double]$m.StandbyCacheNormalPriorityBytes + [double]$m.StandbyCacheReserveBytes) / 1MB, 0) } else { $null })
      modified_mb = $(if ($null -ne $m.ModifiedPageListBytes) { [math]::Round([double]$m.ModifiedPageListBytes / 1MB, 0) } else { $null })
    }
  } catch { return $null }
}
$lists = Get-MemoryLists
if ($null -eq $lists) { $warnings.Add("读不到内存计数器（Win32_PerfRawData_PerfOS_Memory），CSV 里页面读取和页面列表几列会空着") }

$pressureEstimateMB = $null
if ($usePressure -and $null -ne $memory) {
  $physicalRoomMB = $memory.avail_mb - $MarginMB
  $commitRoomMB = $memory.commit_avail_mb - $CommitMarginMB
  $pressureEstimateMB = [math]::Floor([math]::Min($physicalRoomMB, $commitRoomMB))
  $pressureLimit = "可用物理内存"
  if ($commitRoomMB -lt $physicalRoomMB) { $pressureLimit = "可用提交" }
  if ($MaxPressureMB -gt 0 -and $MaxPressureMB -lt $pressureEstimateMB) { $pressureEstimateMB = $MaxPressureMB; $pressureLimit = "-MaxPressureMB" }
  $pressureLeftMB = $memory.avail_mb - [math]::Max(0, $pressureEstimateMB)
  if ($pressureLimit -ne "可用物理内存" -and $pressureLeftMB -gt $MarginMB + 1024) {
    $warnings.Add("pressure 受${pressureLimit}限制，只能占约 $([math]::Max(0, $pressureEstimateMB)) MB，占完后可用物理内存还有约 $pressureLeftMB MB，压不到安全余量 $MarginMB MB，算不上内存紧张。要更紧，先关掉占内存多的程序（尤其是提交量大的），或加大页面文件；不建议调小 -CommitMarginMB")
  }
  if ($pressureEstimateMB -lt $script:ChunkMB) {
    $warnings.Add("按现在的内存，pressure 几乎占不了内存（估算 $pressureEstimateMB MB），施压不会有效果")
  }
}

$running = @(Get-Process -Name "nianlai" -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
  $ids = ($running | ForEach-Object { $_.Id }) -join ", "
  $text = "已有 nianlai.exe 在运行（PID $ids）。两个样机会共用同一个 WebView2 用户数据目录（%LOCALAPPDATA%\app.nianlai\EBWebView）和退出快捷键，请先按 Ctrl+Alt+Shift+Q 退出它"
  if ($AllowExisting) { $warnings.Add($text + "；已加 -AllowExisting，仍会继续") } else { $problems.Add($text + "；确实要同时运行，加 -AllowExisting") }
}

# ---------- 打印计划（DryRun 和正式运行都打印） ----------
$floorMB = [math]::Max(512, [math]::Floor($MarginMB / 2))
$helperMinutes = [math]::Min([double]$PressureMaxMinutes, $pressureSeconds / 60.0 + 5)
$dataDir = Join-Path $OutDir "data"
$childEnvSet = [ordered]@{
  NIANLAI_G0_AUTOCYCLE = $planText
  NIANLAI_G0_DATA_DIR = $dataDir
  NIANLAI_HOTKEY = $Hotkey
}
$childEnvRemoved = @("NIANLAI_MEMORY_TIER", "NIANLAI_G0_RESUSPEND_MIN", "NIANLAI_G0_SAMPLES")

$planLines = New-Object System.Collections.Generic.List[string]
if ($DryRun) { $planLines.Add("=== 内存紧张时的呼出耗时：计划（DryRun，没有启动任何进程，也没有施压）===") }
else { $planLines.Add("=== 内存紧张时的呼出耗时：计划 ===") }
if ($usePressure) { $planLines.Add("模式：pressure（先 none 与 trim 交替，最后一段 pressure）") }
else { $planLines.Add("模式：trim（none 与 trim 交替）") }
if (Test-Path -LiteralPath $Exe -PathType Leaf) {
  $exeItem = Get-Item -LiteralPath $Exe
  $planLines.Add(("样机：{0}（{1} MB，修改于 {2:yyyy-MM-dd HH:mm}）" -f $exeItem.FullName, (Format-Num ($exeItem.Length / 1MB) "0.0"), $exeItem.LastWriteTime))
} else { $planLines.Add("样机：$Exe（不存在）") }
$countText = "none $Cycles 次、trim $Cycles 次"
if ($usePressure) { $countText += "、pressure $Cycles 次" }
$planLines.Add("每组次数：$countText；另有预热 $WarmupCycles 次，不计入统计")
$planLines.Add(("每次：隐藏 {0} 秒 → 显示 {1} 秒；trim 在显示前 {2} 秒修剪整棵进程树" -f (Format-Num $HiddenSeconds "0.##"), (Format-Num $VisibleSeconds "0.##"), (Format-Num $TrimLeadSeconds "0.##")))
$planLines.Add(("总步数：{0}；预计耗时约 {1} 分钟（含启动和收尾），这段时间请离开电脑" -f $steps.Count, (Format-Num ($totalSeconds / 60.0) "0")))
if ($null -ne $memory) {
  $line = "当前内存：物理 $($memory.total_mb) MB，可用 $($memory.avail_mb) MB；提交上限 $($memory.commit_limit_mb) MB，可用提交 $($memory.commit_avail_mb) MB"
  if ($null -ne $lists -and $null -ne $lists.free_zero_mb) { $line += "；空闲页 $($lists.free_zero_mb) MB，备用页 $($lists.standby_mb) MB，修改页 $($lists.modified_mb) MB" }
  $planLines.Add($line)
}
if ($usePressure) {
  $estimateText = "未知"
  if ($null -ne $pressureEstimateMB) { $estimateText = "约 $([math]::Max(0, $pressureEstimateMB)) MB（受${pressureLimit}限制），占完后可用物理内存约 $pressureLeftMB MB" }
  $planLines.Add(("pressure：在第一个 pressure 步的隐藏期开始占内存（预留 {0} 秒），按当时的「可用物理内存 - {1} MB」和「可用提交 - {2} MB」取小者；照现在的数字估算 {3}" -f $RampSeconds, $MarginMB, $CommitMarginMB, $estimateText))
  $planLines.Add(("pressure：每次压缩之后补压到安全余量，累计不超过 {0} MB；可用内存低于 {1} MB 时立即释放；辅助进程最长存活 {2} 分钟，主脚本退出或出错时也会释放" -f $TopUpCapMB, $floorMB, (Format-Num $helperMinutes "0")))
  $planLines.Add(("pressure 阶段预计 {0} 分钟" -f (Format-Num ($pressureSeconds / 60.0) "0.0")))
}
$previewCount = [math]::Min(4, $steps.Count)
$preview = (($planText -split ",") | Select-Object -First $previewCount) -join ","
$planLines.Add("样机环境变量：NIANLAI_G0_AUTOCYCLE=$preview…（共 $($steps.Count) 步，$($planText.Length) 个字符）")
$planLines.Add("              NIANLAI_G0_DATA_DIR=$dataDir；NIANLAI_HOTKEY=$Hotkey；删除 $($childEnvRemoved -join '、')")
if ($DryRun) { $planLines.Add("输出目录：$OutDir（DryRun 不创建）") } else { $planLines.Add("输出目录：$OutDir") }
if ($AllowSleep) { $planLines.Add("显示器和睡眠：不干预（-AllowSleep）；显示器关掉或锁屏后的计时不可信") }
else { $planLines.Add("显示器和睡眠：运行期间阻止睡眠、保持显示器开着；离开时不要锁屏（Win+L）") }
if ($running.Count -eq 0) { $planLines.Add("已有 nianlai.exe：无") }
foreach ($w in $warnings) { $planLines.Add("[警告] $w") }
foreach ($p in $problems) { $planLines.Add("[问题] $p") }
$planLines | ForEach-Object { Write-Host $_ }

if ($problems.Count -gt 0) {
  Write-Host "有 $($problems.Count) 个问题，没有开始。"
  exit 1
}
if ($DryRun) {
  Write-Host "检查通过。去掉 -DryRun 即正式运行。"
  exit 0
}

# ---------- 正式运行：状态 ----------
$script:LogPath = Join-Path $dataDir "panel-log.jsonl"
$script:LogOffset = [long]0
$script:LogPending = New-Object byte[] 0
$script:RootPid = 0
$script:RootStart = $null
$script:KnownTree = @{}
$script:LastTreeAt = [datetime]::MinValue
$script:HelperPid = 0
$script:HelperStart = $null
$script:TopUpRequests = 0
$script:AwakeSet = $false
$script:AutocycleEnabled = $false
$script:DoneFlag = $false
$script:AbortReason = $null
$script:EndedReason = "被中断（Ctrl+C 或异常）"
$script:SuspendCount = 0
$script:IgnoredCommands = 0
$script:Schedule = @{}
$script:Rows = @{}
$script:FirstPressureStep = 0
$probeLeadMs = 1500
for ($k = 1; $k -le $steps.Count; $k++) {
  $condition = $steps[$k - 1].Condition
  if ($condition -eq "pressure" -and $script:FirstPressureStep -eq 0) { $script:FirstPressureStep = $k }
  $script:Rows[$k] = [ordered]@{
    cycle = $k; condition = $condition; hidden_planned_s = $steps[$k - 1].Hidden; hidden_ms = $null
    trigger_to_show_ms = $null; webview_visible_ms = $null; show_to_paint_ms = $null; trigger_to_paint_ms = $null; painted = $false
    trim_processes = $null; trim_failed = $null; trim_errors = $null; trim_before_mb = $null; trim_after_mb = $null
    avail_mb = $null; commit_avail_mb = $null; free_zero_mb = $null; standby_mb = $null; modified_mb = $null
    tree_private_ws_mb = $null; tree_processes = $null; webview_mem_priority = $null; helper_state = $null; helper_mb = $null
    faults_delta = $null; page_reads_delta = $null; pages_input_delta = $null; note = ""
    _before = $null; _shown = $false
  }
}

function Add-Note([int]$Cycle, [string]$Text) {
  $row = $script:Rows[$Cycle]
  if ($null -eq $row) { return }
  if ([string]::IsNullOrEmpty($row.note)) { $row.note = $Text } else { $row.note = $row.note + ";" + $Text }
}

# 只读新增的部分；只在最后一个换行处切开，避免把一个多字节字符拆成两半。
function Read-NewEvents {
  $events = New-Object System.Collections.Generic.List[object]
  if (-not (Test-Path -LiteralPath $script:LogPath)) { return $events.ToArray() }
  $stream = $null
  try {
    $share = [System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete
    $stream = New-Object System.IO.FileStream($script:LogPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, $share)
    $length = $stream.Length
    if ($length -le $script:LogOffset) { return $events.ToArray() }
    [void]$stream.Seek($script:LogOffset, [System.IO.SeekOrigin]::Begin)
    $count = [int]($length - $script:LogOffset)
    $buffer = New-Object byte[] ($script:LogPending.Length + $count)
    [Array]::Copy($script:LogPending, 0, $buffer, 0, $script:LogPending.Length)
    $read = 0
    while ($read -lt $count) {
      $n = $stream.Read($buffer, $script:LogPending.Length + $read, $count - $read)
      if ($n -le 0) { break }
      $read += $n
    }
    $script:LogOffset += $read
    $total = $script:LogPending.Length + $read
    $lastNewline = [Array]::LastIndexOf($buffer, [byte]10, $total - 1)
    if ($lastNewline -lt 0) {
      $script:LogPending = New-Object byte[] $total
      [Array]::Copy($buffer, 0, $script:LogPending, 0, $total)
      return $events.ToArray()
    }
    $text = $script:Utf8NoBom.GetString($buffer, 0, $lastNewline + 1)
    $rest = $total - $lastNewline - 1
    $script:LogPending = New-Object byte[] $rest
    if ($rest -gt 0) { [Array]::Copy($buffer, $lastNewline + 1, $script:LogPending, 0, $rest) }
    foreach ($line in ($text -split "`n")) {
      $trimmed = $line.Trim()
      if ($trimmed.Length -eq 0) { continue }
      try { $events.Add(($trimmed | ConvertFrom-Json)) } catch { }
    }
  } catch {
  } finally {
    if ($null -ne $stream) { $stream.Dispose() }
  }
  return $events.ToArray()
}

function Invoke-Event($Evt) {
  $name = [string]$Evt.event
  switch ($name) {
    "autocycle_enabled" { $script:AutocycleEnabled = $true }
    "autocycle_invalid" { $script:AbortReason = "样机拒绝了自动循环计划：$($Evt.reason)" }
    "autocycle_aborted" { $script:AbortReason = "样机中止了自动循环：$($Evt.reason)" }
    "panel_error" { $script:AbortReason = "样机报错：$($Evt.error)" }
    "webview_failed" { $script:AbortReason = "WebView2 显示或隐藏失败：$($Evt.error)" }
    "autocycle_command_ignored" { $script:IgnoredCommands++ }
    "autocycle_done" { $script:DoneFlag = $true }
    "webview_suspended" {
      if ($Evt.stale -eq $true) { return }
      # 第 k 次挂起回执就是第 k 步隐藏计时的起点：样机在这之后等满该步的隐藏秒数再显示。
      $script:SuspendCount++
      $k = $script:SuspendCount
      if ($k -le $steps.Count) {
        $receipt = [long]$Evt.time_ms
        $script:Schedule[$k] = @{ Receipt = $receipt; Trigger = $receipt + [long]($steps[$k - 1].Hidden * 1000); Done = @{} }
        if ($Evt.suspend_result -ne $true) { Add-Note $k "suspend_failed" }
      }
    }
    "shown" {
      if ($Evt.auto -ne $true -or $null -eq $Evt.cycle) { return }
      $row = $script:Rows[[int]$Evt.cycle]
      if ($null -eq $row) { return }
      $row._shown = $true
      $row.hidden_ms = $Evt.hidden_ms
      $row.trigger_to_show_ms = $Evt.hotkey_to_show_ms
      $row.webview_visible_ms = $Evt.webview_visible_ms
    }
    "painted" {
      if ($Evt.auto -ne $true -or $null -eq $Evt.cycle) { return }
      $row = $script:Rows[[int]$Evt.cycle]
      if ($null -eq $row) { return }
      $row.painted = $true
      $row.show_to_paint_ms = $Evt.show_to_paint_ms
      $row.trigger_to_paint_ms = $Evt.trigger_to_paint_ms
    }
  }
}

# 样机进程树：每次都重新枚举；新出现的 PID 记下名字和启动时间，收尾时只结束这些进程。
function Get-Tree {
  $names = $null
  $ids = [NianlaiShowNative]::Tree([uint32]$script:RootPid, [ref]$names)
  for ($i = 0; $i -lt $ids.Length; $i++) {
    $id = [uint32]$ids[$i]
    if (-not $script:KnownTree.ContainsKey($id)) {
      $start = $null
      try { $start = (Get-Process -Id $id -ErrorAction Stop).StartTime } catch { }
      $script:KnownTree[$id] = [pscustomobject]@{ Name = $names[$i]; Start = $start }
    }
  }
  $script:LastTreeAt = Get-Date
  return [pscustomobject]@{ Ids = $ids; Names = $names }
}

function Get-HelperState {
  $path = Join-Path $OutDir "helper-status.jsonl"
  if (-not (Test-Path -LiteralPath $path)) { return $null }
  try {
    # 辅助进程随时在追加，用允许写入的共享方式打开，免得两边互相挡住。
    $stream = New-Object System.IO.FileStream($path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
    try { $reader = New-Object System.IO.StreamReader($stream, $script:Utf8NoBom); $content = $reader.ReadToEnd() } finally { $stream.Dispose() }
    $lines = $content -split "`n"
    for ($i = $lines.Length - 1; $i -ge 0; $i--) {
      if ($lines[$i].Trim().Length -gt 0) { return ($lines[$i] | ConvertFrom-Json) }
    }
  } catch { }
  return $null
}

# 显示前后各采一次。前一次在显示前 1.5 秒（修剪之后），后一次在显示阶段快结束时。
function Get-Probe {
  $tree = Get-Tree
  $sample = [NianlaiShowNative]::Sample([uint32[]]$tree.Ids)
  $status = [NianlaiShowNative]::MemoryStatus()
  $probe = [ordered]@{
    avail_mb = [math]::Round($status[0] / 1MB, 0)
    commit_avail_mb = [math]::Round($status[2] / 1MB, 0)
    tree_private_ws_mb = [math]::Round($sample[0] / 1MB, 1)
    tree_faults = $sample[1]
    tree_processes = $tree.Ids.Length
    page_reads = $null; pages_input = $null; free_zero_mb = $null; standby_mb = $null; modified_mb = $null
    webview_mem_priority = $null
  }
  $memoryLists = Get-MemoryLists
  if ($null -ne $memoryLists) { foreach ($key in $memoryLists.Keys) { $probe[$key] = $memoryLists[$key] } }
  $priorities = @()
  for ($i = 0; $i -lt $tree.Ids.Length; $i++) {
    if ($tree.Names[$i] -eq "msedgewebview2.exe") {
      $value = [NianlaiShowNative]::MemoryPriority([uint32]$tree.Ids[$i])
      if ($value -ge 0) { $priorities += $value }
    }
  }
  if ($priorities.Count -gt 0) {
    $sorted = @($priorities | Sort-Object)
    $probe.webview_mem_priority = "{0}-{1}" -f $sorted[0], $sorted[$sorted.Count - 1]
  }
  return $probe
}

function Invoke-Trim([int]$Cycle) {
  $row = $script:Rows[$Cycle]
  $tree = Get-Tree
  $before = [NianlaiShowNative]::Sample([uint32[]]$tree.Ids)
  $failed = 0
  $errors = @()
  foreach ($id in $tree.Ids) {
    $code = [NianlaiShowNative]::Trim([uint32]$id)
    if ($code -ne 0) { $failed++; $errors += "$id=$code" }
  }
  $after = [NianlaiShowNative]::Sample([uint32[]]$tree.Ids)
  $row.trim_processes = $tree.Ids.Length
  $row.trim_failed = $failed
  $row.trim_errors = ($errors -join " ")
  $row.trim_before_mb = [math]::Round($before[0] / 1MB, 1)
  $row.trim_after_mb = [math]::Round($after[0] / 1MB, 1)
}

function Start-PressureHelper {
  $config = [ordered]@{
    margin_mb = $MarginMB; commit_margin_mb = $CommitMarginMB; floor_mb = $floorMB
    max_pressure_mb = $MaxPressureMB; topup_cap_mb = $TopUpCapMB
    # 按字符串传，避免 JSON 把大整数读成浮点数后对不上。
    parent_start_ticks = [string](Get-Process -Id $PID).StartTime.ToUniversalTime().Ticks
  }
  [System.IO.File]::WriteAllText((Join-Path $OutDir "helper-config.json"), ($config | ConvertTo-Json), $script:Utf8NoBom)
  $deadline = (Get-NowMs) + [long]($helperMinutes * 60000)
  $hostExe = (Get-Process -Id $PID).Path
  # PowerShell 5.1 的 Start-Process 不会替数组参数加引号，这里自己拼好整条命令行。
  $arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`" -PressureHelper -HelperParentPid $PID -HelperDir `"$OutDir`" -HelperDeadlineMs $deadline"
  $helper = Start-Process -FilePath $hostExe -ArgumentList $arguments -WindowStyle Hidden -PassThru
  if ($null -eq $helper -or $helper.Id -le 0) { throw "辅助进程没有启动成功" }
  $script:HelperPid = $helper.Id
  try { $script:HelperStart = $helper.StartTime } catch { }
  $script:RunInfo["helper_pid"] = $script:HelperPid
  Save-RunInfo
  Write-Host "已启动占内存的辅助进程（PID $($script:HelperPid)），最长存活 $(Format-Num $helperMinutes '0') 分钟。"
}

# run.json 记参数、环境和进程号；脚本窗口被关掉时，可以按里面的 pid 手工停样机。
function Save-RunInfo {
  if ($null -eq $script:RunInfo) { return }
  try { [System.IO.File]::WriteAllText((Join-Path $OutDir "run.json"), ($script:RunInfo | ConvertTo-Json -Depth 4), $script:Utf8NoBom) } catch { }
}

function Request-TopUp {
  if ($script:HelperPid -le 0) { return $true }
  $script:TopUpRequests++
  # 辅助进程可能正在读这个文件，写失败就稍等重试；三次都失败只记一笔，不中断测量。
  for ($attempt = 1; $attempt -le 3; $attempt++) {
    try { [System.IO.File]::WriteAllText((Join-Path $OutDir "helper-control.txt"), [string]$script:TopUpRequests, $script:Utf8NoBom); return $true } catch { Start-Sleep -Milliseconds 50 }
  }
  return $false
}

function Stop-Helper {
  if ($script:HelperPid -le 0) { return }
  try { [System.IO.File]::WriteAllText((Join-Path $OutDir "helper-stop"), "stop", $script:Utf8NoBom) } catch { }
  if ($null -ne (Get-Process -Id $script:HelperPid -ErrorAction SilentlyContinue)) {
    try { Wait-Process -Id $script:HelperPid -Timeout 10 -ErrorAction SilentlyContinue } catch { }
    $left = Get-Process -Id $script:HelperPid -ErrorAction SilentlyContinue
    # PID 可能已被复用：只结束启动时间对得上的那个进程。进程一结束，它占的内存就全部回到系统。
    if ($null -ne $left -and ($null -eq $script:HelperStart -or $left.StartTime -eq $script:HelperStart)) {
      Stop-Process -Id $script:HelperPid -Force -ErrorAction SilentlyContinue
    }
  }
}

function Stop-Prototype {
  if ($script:RootPid -le 0) { return }
  $root = Get-Process -Id $script:RootPid -ErrorAction SilentlyContinue
  if ($null -ne $root -and ($null -eq $script:RootStart -or $root.StartTime -eq $script:RootStart)) {
    # 不带 /F 的 taskkill 只发关闭消息，样机会正常退出并写完日志；15 秒后还在才强制结束。
    & taskkill.exe /PID $script:RootPid 2>&1 | Out-Null
    try { Wait-Process -Id $script:RootPid -Timeout 15 -ErrorAction SilentlyContinue } catch { }
    $root = Get-Process -Id $script:RootPid -ErrorAction SilentlyContinue
    if ($null -ne $root -and ($null -eq $script:RootStart -or $root.StartTime -eq $script:RootStart)) {
      Stop-Process -Id $script:RootPid -Force -ErrorAction SilentlyContinue
    }
  }
  # WebView2 子进程通常随主进程退出；还在的只按记录过的 PID 结束，且名字和启动时间都要对得上。
  $waitUntil = (Get-Date).AddSeconds(10)
  $children = @($script:KnownTree.Keys | Where-Object { $_ -ne [uint32]$script:RootPid })
  while ((Get-Date) -lt $waitUntil) {
    $alive = @($children | Where-Object { $null -ne (Get-Process -Id $_ -ErrorAction SilentlyContinue) })
    if ($alive.Count -eq 0) { break }
    Start-Sleep -Milliseconds 500
  }
  foreach ($id in $children) {
    $known = $script:KnownTree[$id]
    $left = Get-Process -Id $id -ErrorAction SilentlyContinue
    if ($null -ne $left -and $left.ProcessName -eq "msedgewebview2" -and $null -ne $known.Start -and $left.StartTime -eq $known.Start) {
      Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
      $script:LeftoverKilled += [string]$id
    }
  }
}

# ---------- 结果：CSV 和中文摘要 ----------
# 线性插值的分位数（和 Excel 的 PERCENTILE.INC 一致）；样本少时 P95 接近最大值。
function Get-Percentile([double[]]$Values, [double]$P) {
  if ($null -eq $Values -or $Values.Count -eq 0) { return $null }
  $sorted = [double[]]@($Values | Sort-Object)
  if ($sorted.Count -eq 1) { return $sorted[0] }
  $h = ($sorted.Count - 1) * $P
  $lo = [int][math]::Floor($h)
  $hi = [math]::Min($lo + 1, $sorted.Count - 1)
  return $sorted[$lo] + ($h - $lo) * ($sorted[$hi] - $sorted[$lo])
}

function Format-Ms($Value) {
  if ($null -eq $Value) { return "无" }
  return (Format-Num ([double]$Value) "0.0") + " ms"
}

function Format-Count($Value, [string]$Pattern = "0") {
  if ($null -eq $Value) { return "无" }
  return (Format-Num ([double]$Value) $Pattern)
}

function Format-Cell($Value) {
  if ($null -eq $Value) { return "" }
  if ($Value -is [bool]) { if ($Value) { return "true" } else { return "false" } }
  if ($Value -is [double] -or $Value -is [single] -or $Value -is [decimal]) { return (Format-Num ([double]$Value) "0.###") }
  $text = [string]$Value
  if ($text.Contains(",") -or $text.Contains('"')) { return '"' + $text.Replace('"', '""') + '"' }
  return $text
}

function Get-Stats($Rows, [string]$Field) {
  $values = [double[]]@($Rows | Where-Object { $null -ne $_[$Field] } | ForEach-Object { [double]$_[$Field] })
  return [pscustomobject]@{
    N = $values.Count
    Median = (Get-Percentile $values 0.5)
    P95 = (Get-Percentile $values 0.95)
    Max = $(if ($values.Count -gt 0) { ($values | Measure-Object -Maximum).Maximum } else { $null })
  }
}

function Write-Results {
  $fields = @("cycle", "condition", "hidden_planned_s", "hidden_ms", "trigger_to_show_ms", "webview_visible_ms", "show_to_paint_ms", "trigger_to_paint_ms", "painted",
    "trim_processes", "trim_failed", "trim_errors", "trim_before_mb", "trim_after_mb", "avail_mb", "commit_avail_mb", "free_zero_mb", "standby_mb", "modified_mb",
    "tree_private_ws_mb", "tree_processes", "webview_mem_priority", "helper_state", "helper_mb", "faults_delta", "page_reads_delta", "pages_input_delta", "note")
  $lines = New-Object System.Collections.Generic.List[string]
  $lines.Add($fields -join ",")
  for ($k = 1; $k -le $steps.Count; $k++) {
    $row = $script:Rows[$k]
    $lines.Add((($fields | ForEach-Object { Format-Cell $row[$_] }) -join ","))
  }
  [System.IO.File]::WriteAllText((Join-Path $OutDir "shows.csv"), (($lines -join "`r`n") + "`r`n"), $script:Utf8NoBom)

  $summary = New-Object System.Collections.Generic.List[string]
  $summary.Add("开始：" + $script:StartedAt.ToString("yyyy-MM-dd HH:mm:ss") + "　结束：" + $script:EndedAt.ToString("yyyy-MM-dd HH:mm:ss"))
  $summary.Add("结束原因：" + $script:EndedReason)
  $summary.Add("模式：$Mode；计划 $($steps.Count) 步（预热 $WarmupCycles 步），隐藏 $(Format-Num $HiddenSeconds '0.##') 秒、显示 $(Format-Num $VisibleSeconds '0.##') 秒；样机 PID $($script:RootPid)")
  $summary.Add("口径：触发到首帧 = 样机自动显示的触发时刻到前端两次 requestAnimationFrame 之后回报的时刻（painted 事件的 trigger_to_paint_ms）。")
  $summary.Add("PRD 门槛（第 7 节「呼出速度」，第 5 节第 1 条）：暖启动 P95 ≤ 200 ms，目标 100 ms，计时 100 次。")
  $summary.Add("")
  $base = $null
  foreach ($condition in @("none", "trim", "pressure")) {
    $all = @($script:Rows.Values | Where-Object { $_.condition -eq $condition })
    if ($all.Count -eq 0) { continue }
    $shown = @($all | Where-Object { $_._shown })
    $unpainted = @($shown | Where-Object { -not $_.painted })
    # 没画出来的按显示时长计入：真实值只会更大，所以这样算出的分位数是偏乐观的下限。
    foreach ($row in $unpainted) { $row["_paint_for_stats"] = $VisibleSeconds * 1000.0 }
    foreach ($row in @($shown | Where-Object { $_.painted })) { $row["_paint_for_stats"] = [double]$row.trigger_to_paint_ms }
    $paint = Get-Stats $shown "_paint_for_stats"
    $toShow = Get-Stats $shown "trigger_to_show_ms"
    $toPaint = Get-Stats @($shown | Where-Object { $_.painted }) "show_to_paint_ms"
    $visible = Get-Stats $shown "webview_visible_ms"
    $faults = Get-Stats $shown "faults_delta"
    $reads = Get-Stats $shown "page_reads_delta"
    $avail = Get-Stats $shown "avail_mb"
    $summary.Add("【$condition】计划 $($all.Count) 次，显示 $($shown.Count) 次，其中没画出来 $($unpainted.Count) 次（按 $(Format-Num ($VisibleSeconds * 1000) '0') ms 计入）")
    $summary.Add("  触发到首帧：中位数 $(Format-Ms $paint.Median)，P95 $(Format-Ms $paint.P95)，最大 $(Format-Ms $paint.Max)")
    $summary.Add("  分两段：触发到显示 中位数 $(Format-Ms $toShow.Median)（P95 $(Format-Ms $toShow.P95)），其中 SetIsVisible 加恢复 中位数 $(Format-Ms $visible.Median)；显示到首帧 中位数 $(Format-Ms $toPaint.Median)（P95 $(Format-Ms $toPaint.P95)）")
    $summary.Add("  显示前后：样机进程树缺页 中位数 $(Format-Count $faults.Median) 次；系统页面读取 中位数 $(Format-Count $reads.Median) 次（P95 $(Format-Count $reads.P95)）；显示前可用内存 中位数 $(Format-Count $avail.Median) MB")
    if ($condition -eq "trim") {
      $trimmed = @($all | Where-Object { $null -ne $_.trim_processes })
      $failedTotal = 0
      $processTotal = 0
      foreach ($r in $trimmed) { $failedTotal += [int]$r.trim_failed; $processTotal += [int]$r.trim_processes }
      $before = Get-Stats $trimmed "trim_before_mb"
      $after = Get-Stats $trimmed "trim_after_mb"
      $summary.Add("  修剪：$($trimmed.Count) 次，共 $processTotal 个进程次，失败 $failedTotal 个进程次；修剪前后专用工作集 中位数 $(Format-Count $before.Median 0.0) → $(Format-Count $after.Median 0.0) MB")
      $errorSamples = @($trimmed | Where-Object { $_.trim_failed -gt 0 } | Select-Object -First 3 | ForEach-Object { $_.trim_errors })
      if ($errorSamples.Count -gt 0) { $summary.Add("  修剪失败举例（PID=错误码，5 为拒绝访问）：" + ($errorSamples -join " | ")) }
    }
    if ($condition -eq "pressure") {
      $helperMb = Get-Stats $shown "helper_mb"
      $notReady = @($all | Where-Object { $_.note -match "pressure_not_ready|helper_gone" }).Count
      $summary.Add("  辅助进程占用：中位数 $(Format-Count $helperMb.Median) MB，最大 $(Format-Count $helperMb.Max) MB；未就绪或已退出 $notReady 次")
      if ($null -ne $reads.Median) {
        if ([double]$reads.Median -lt 200) {
          $summary.Add("  注意：显示期间的系统页面读取很少（中位数不到 200 次），WebView2 的页大概没有真的被换出到磁盘，这组结果更接近「内存紧张但页还在内存里（备用列表或压缩存储）」。")
        } else {
          $summary.Add("  显示期间有明显的页面读取，说明有页是从磁盘读回的。")
        }
      }
    }
    if ($condition -eq "none") { $base = $paint }
    elseif ($null -ne $base -and $null -ne $base.Median -and $null -ne $paint.Median) {
      $summary.Add([string]::Format($script:Invariant, "  对比 none：中位数 {0:+0.0;-0.0;0.0} ms，P95 {1:+0.0;-0.0;0.0} ms", ([double]$paint.Median - [double]$base.Median), ([double]$paint.P95 - [double]$base.P95)))
    }
    if ($null -ne $paint.P95) {
      $verdict = "未达门槛（P95 > 200 ms）"
      if ([double]$paint.P95 -le 100) { $verdict = "达到门槛，也达到目标（P95 ≤ 100 ms）" }
      elseif ([double]$paint.P95 -le 200) { $verdict = "达到门槛（P95 ≤ 200 ms），未达目标 100 ms" }
      $sampleNote = ""
      if ($paint.N -lt 100) { $sampleNote = "；样本 $($paint.N) 次，少于 PRD 规定的 100 次，P95 只作参考" }
      $summary.Add("  按 PRD：$verdict$sampleNote")
    }
    $summary.Add("")
  }

  $statusPath = Join-Path $OutDir "helper-status.jsonl"
  if (Test-Path -LiteralPath $statusPath) {
    $states = @([System.IO.File]::ReadAllLines($statusPath) | Where-Object { $_.Trim().Length -gt 0 } | ForEach-Object { try { $_ | ConvertFrom-Json } catch { } })
    $maxAllocated = ($states | Measure-Object -Property allocated_mb -Maximum).Maximum
    $minAvail = ($states | Where-Object { $null -ne $_.avail_mb } | Measure-Object -Property avail_mb -Minimum).Minimum
    $emergency = @($states | Where-Object { $_.state -eq "emergency_release" }).Count
    $stopped = @($states | Where-Object { $_.state -like "*_stopped" } | ForEach-Object { $_.reason }) | Select-Object -Unique
    $last = $states | Select-Object -Last 1
    $summary.Add("辅助进程：最多占用 $maxAllocated MB；它记录到的最低可用内存 $minAvail MB；紧急释放 $emergency 次；分配提前停止的原因：$(if ($stopped) { $stopped -join '、' } else { '无' })；最后状态：$($last.state) $($last.reason)")
  }
  $summary.Add("自动模式下被忽略的前端命令：$($script:IgnoredCommands) 次（不为 0 说明测试期间有人点了面板）")
  $locked = @($script:Rows.Values | Where-Object { $_.note -match "screen_locked" }).Count
  if ($locked -gt 0) { $summary.Add("有 $locked 步显示时处于锁屏状态（note 列 screen_locked），这些步的耗时不可信，建议整轮重跑。") }
  $notes = @(1..$steps.Count | ForEach-Object { $script:Rows[$_] } | Where-Object { -not [string]::IsNullOrEmpty($_.note) } | ForEach-Object { "第 $($_.cycle) 步：$($_.note)" })
  if ($notes.Count -gt 0) { $summary.Add("异常记录（详见 shows.csv 的 note 列）：" + (($notes | Select-Object -First 10) -join "；")) }
  if ($script:LeftoverKilled.Count -gt 0) { $summary.Add("强制结束的残留 WebView2 进程 PID：" + ($script:LeftoverKilled -join ", ")) }
  $summary.Add("data 目录是样机这次用的临时数据目录；测试期间如果有人复制过内容，历史库里会有记录，不需要可以整个删除。")
  [System.IO.File]::WriteAllText((Join-Path $OutDir "summary.txt"), (($summary -join "`r`n") + "`r`n"), $script:Utf8NoBom)
  $summary | ForEach-Object { Write-Host $_ }
}

$script:LeftoverKilled = @()
$script:RunInfo = $null
$script:StartedAt = Get-Date
try {
  New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
  Initialize-Native

  # ---------- run.json ----------
  $webview2Version = $null
  foreach ($key in @("HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}", "HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}")) {
    if ($null -eq $webview2Version -and (Test-Path $key)) {
      $pv = (Get-ItemProperty -Path $key -ErrorAction SilentlyContinue).pv
      if ($pv) { $webview2Version = [string]$pv }
    }
  }
  $exePath = (Resolve-Path -LiteralPath $Exe).Path
  $osInfo = Get-CimInstance Win32_OperatingSystem
  $script:RunInfo = [ordered]@{
    label = $Label; mode = $Mode; cycles = $Cycles; warmup_cycles = $WarmupCycles
    hidden_seconds = $HiddenSeconds; visible_seconds = $VisibleSeconds; trim_lead_seconds = $TrimLeadSeconds
    margin_mb = $MarginMB; commit_margin_mb = $CommitMarginMB; floor_mb = $floorMB; max_pressure_mb = $MaxPressureMB
    topup_cap_mb = $TopUpCapMB; topup_after_seconds = $TopUpAfterSeconds; ramp_seconds = $RampSeconds; pressure_max_minutes = $PressureMaxMinutes
    started_at = $script:StartedAt.ToString("yyyy-MM-ddTHH:mm:ss"); hostname = $env:COMPUTERNAME
    os = [ordered]@{ caption = [string]$osInfo.Caption; version = [string]$osInfo.Version; build = [string]$osInfo.BuildNumber }
    memory_at_start = $memory; memory_lists_at_start = $lists
    memory_compression_process = ($null -ne (Get-Process -Name "Memory Compression" -ErrorAction SilentlyContinue))
    webview2_runtime_version = $webview2Version
    webview2_additional_browser_arguments = $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS
    powershell = $PSVersionTable.PSVersion.ToString()
    exe_path = $exePath
    exe_sha256 = (Get-FileHash -LiteralPath $exePath -Algorithm SHA256).Hash
    exe_last_write_utc = (Get-Item -LiteralPath $exePath).LastWriteTimeUtc.ToString("yyyy-MM-ddTHH:mm:ssZ")
    child_env_set = $childEnvSet; child_env_removed = $childEnvRemoved
    steps = $steps.Count
  }
  Save-RunInfo

  if (-not $AllowSleep) { [NianlaiShowNative]::KeepAwake($true); $script:AwakeSet = $true }

  # ---------- 启动样机：环境变量只在启动的一瞬间改，之后立刻恢复本进程的环境 ----------
  $backup = @{}
  foreach ($name in @($childEnvRemoved) + @($childEnvSet.Keys)) { $backup[$name] = [Environment]::GetEnvironmentVariable($name, "Process") }
  try {
    foreach ($name in $childEnvRemoved) { [Environment]::SetEnvironmentVariable($name, $null, "Process") }
    foreach ($name in $childEnvSet.Keys) { [Environment]::SetEnvironmentVariable($name, [string]$childEnvSet[$name], "Process") }
    $app = Start-Process -FilePath $exePath -WorkingDirectory (Split-Path -Parent $exePath) -PassThru
  } finally {
    foreach ($name in $backup.Keys) { [Environment]::SetEnvironmentVariable($name, $backup[$name], "Process") }
  }
  if ($null -eq $app -or $app.Id -le 0) { throw "样机没有启动成功：$exePath" }
  $script:RootPid = $app.Id
  try { $script:RootStart = $app.StartTime } catch { }
  $script:KnownTree[[uint32]$app.Id] = [pscustomobject]@{ Name = "nianlai.exe"; Start = $script:RootStart }
  $script:RunInfo["pid"] = $script:RootPid
  Save-RunInfo
  Write-Host "已启动样机（PID $($script:RootPid)）。主显示器右下角会反复闪出面板，不用管；要提前停就在这个窗口按一次 Ctrl+C。"

  $startMs = Get-NowMs
  $deadlineMs = $startMs + [long](($totalSeconds + 600) * 1000)
  $measured = @($steps | Where-Object { $_.Condition -ne "warmup" }).Count
  $finishedCount = 0

  # ---------- 主循环：读日志，按每一步的预定显示时刻安排修剪、补压和采样 ----------
  while ($true) {
    foreach ($evt in @(Read-NewEvents)) { Invoke-Event $evt }
    if ($null -ne $script:AbortReason) { $script:EndedReason = "中止：" + $script:AbortReason; break }
    if ($script:DoneFlag) { $script:EndedReason = "完成"; break }
    $now = Get-NowMs
    foreach ($k in @($script:Schedule.Keys | Sort-Object)) {
      $entry = $script:Schedule[$k]
      if ($entry.Done.ContainsKey("finished")) { continue }
      $row = $script:Rows[$k]
      $trigger = $entry.Trigger
      if ($k -eq $script:FirstPressureStep -and -not $entry.Done.ContainsKey("ramp") -and $now -ge $entry.Receipt + 1000) {
        $entry.Done["ramp"] = $true
        Start-PressureHelper
      }
      if ($row.condition -eq "pressure" -and -not $entry.Done.ContainsKey("topup") -and $now -ge $trigger - [long](($HiddenSeconds - $TopUpAfterSeconds) * 1000)) {
        $entry.Done["topup"] = $true
        if ($now -lt $trigger - 3000) { if (-not (Request-TopUp)) { Add-Note $k "topup_write_failed" } } else { Add-Note $k "topup_late" }
      }
      if ($row.condition -eq "trim" -and -not $entry.Done.ContainsKey("trim") -and $now -ge $trigger - [long]($TrimLeadSeconds * 1000)) {
        $entry.Done["trim"] = $true
        if ($now -lt $trigger - $probeLeadMs) { Invoke-Trim $k } else { Add-Note $k "trim_late" }
      }
      if (-not $entry.Done.ContainsKey("before") -and $now -ge $trigger - $probeLeadMs) {
        $entry.Done["before"] = $true
        if ($now -lt $trigger - 300) {
          $probe = Get-Probe
          $row._before = $probe
          foreach ($field in @("avail_mb", "commit_avail_mb", "free_zero_mb", "standby_mb", "modified_mb", "tree_private_ws_mb", "tree_processes", "webview_mem_priority")) { $row[$field] = $probe[$field] }
          # 锁屏时 LogonUI 在运行，这时页面的绘制不代表正常使用，记一笔，分析时剔除。
          if ($null -ne (Get-Process -Name "LogonUI" -ErrorAction SilentlyContinue)) { Add-Note $k "screen_locked" }
          if ($row.condition -eq "pressure") {
            $state = Get-HelperState
            if ($null -ne $state) { $row.helper_state = [string]$state.state; $row.helper_mb = $state.allocated_mb }
            $helperAlive = ($script:HelperPid -gt 0 -and $null -ne (Get-Process -Id $script:HelperPid -ErrorAction SilentlyContinue))
            if (-not $helperAlive) { Add-Note $k "helper_gone" }
            elseif ($null -eq $state -or @("ramp_start", "ramp", "released") -contains [string]$state.state) { Add-Note $k "pressure_not_ready" }
          }
        } else { Add-Note $k "probe_late" }
      }
      if (-not $entry.Done.ContainsKey("after") -and $now -ge $trigger + [long]($VisibleSeconds * 1000) - 400) {
        $entry.Done["after"] = $true
        if ($null -ne $row._before) {
          $probe = Get-Probe
          $row.faults_delta = [long]$probe.tree_faults - [long]$row._before.tree_faults
          if ($null -ne $probe.page_reads -and $null -ne $row._before.page_reads) { $row.page_reads_delta = [long]$probe.page_reads - [long]$row._before.page_reads }
          if ($null -ne $probe.pages_input -and $null -ne $row._before.pages_input) { $row.pages_input_delta = [long]$probe.pages_input - [long]$row._before.pages_input }
        }
      }
      if (-not $row._shown -and $now -gt $trigger + [long]($VisibleSeconds * 1000) + 30000) {
        $script:AbortReason = "第 $k 步过了预定时间 30 秒仍没有显示"
      }
      if ($entry.Done.ContainsKey("after") -and $row._shown) {
        $entry.Done["finished"] = $true
        if ($row.condition -ne "warmup") { $finishedCount++ }
        $paintText = "没画出来（显示阶段结束前没收到首帧）"
        if ($row.painted) { $paintText = "触发到首帧 " + (Format-Num ([double]$row.trigger_to_paint_ms) "0.0") + " ms" }
        Write-Host ("[{0:HH:mm:ss}] 第 {1}/{2} 步（{3}）：{4}；可用内存 {5} MB；系统页面读取 {6} 次" -f (Get-Date), $k, $steps.Count, $row.condition, $paintText, $row.avail_mb, $row.page_reads_delta)
      }
    }
    if ($null -ne $script:AbortReason) { $script:EndedReason = "中止：" + $script:AbortReason; break }
    if ($finishedCount -ge $measured -and $script:SuspendCount -gt $steps.Count) { $script:EndedReason = "完成"; break }
    if ($null -eq (Get-Process -Id $script:RootPid -ErrorAction SilentlyContinue)) { $script:EndedReason = "样机进程提前退出"; break }
    if (-not $script:AutocycleEnabled -and $now - $startMs -gt 60000) { $script:EndedReason = "中止：60 秒内没有在日志里看到 autocycle_enabled"; break }
    if ($now -gt $deadlineMs) { $script:EndedReason = "中止：超过计划时长 10 分钟仍未结束"; break }
    Start-Sleep -Milliseconds 50
  }
} catch {
  $script:EndedReason = "出错：" + $_.Exception.Message
  Write-Warning $script:EndedReason
} finally {
  $script:EndedAt = Get-Date
  try { Stop-Helper } catch { Write-Warning "停止辅助进程时出错：$($_.Exception.Message)" }
  try { Stop-Prototype } catch { Write-Warning "停止样机时出错：$($_.Exception.Message)" }
  if ($script:AwakeSet) { try { [NianlaiShowNative]::KeepAwake($false) } catch { } }
  Start-Sleep -Seconds 1
  try {
    if (Test-Path -LiteralPath $script:LogPath) { Copy-Item -LiteralPath $script:LogPath -Destination (Join-Path $OutDir "panel-log.jsonl") -Force }
  } catch { Write-Warning "复制样机日志时出错：$($_.Exception.Message)" }
  try { Write-Results } catch { Write-Warning "写结果时出错：$($_.Exception.Message)" }
  Write-Host "完成，结果在：$OutDir"
}
