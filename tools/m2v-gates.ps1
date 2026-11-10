<#
M2-V beta.2 验收门槛的统一入口。六道门槛各分两层：
  headless  确定性层：cargo 测试（nianlai-store / nianlai），不碰剪贴板，CI 同样会跑。
  real      真实应用层（门槛 1、2、6）：启动 release 样机（隔离数据目录、CDP 端口 9223），
            驱动真实剪贴板，再检查库和文件。会占用你的剪贴板。

用法（在仓库根目录）：
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\m2v-gates.ps1 -Gate all
  ... -Gate 1,2 -Quick          只跑门槛 1、2，小规模冒烟
  ... -Headless                 只跑确定性层（不会碰剪贴板，日常实例开着也能跑）
  ... -DataRoot D:\tmp\m2v      真实层的隔离数据放哪（默认 %TEMP%\nianlai-m2v-<时间>）

铁律：
  - 真实层启动前，只要有任何 nianlai.exe 在运行（尤其是 target\release 下的日常实例）就拒绝。
    原因：单实例插件会把新实例的启动转发给已有实例，等于去操作你的日常实例。
  - 只结束路径含 x86_64-pc-windows-msvc 的、本脚本自己启动的样机；绝不碰其它进程。
  - 不发送任何按键，不用 Ctrl+Alt 组合键，不接触任何真实密码管理器（只用 PR #23 的假进程）。
  - 完整规模（1 万次复制、30 次强杀）会长时间占用剪贴板，请在你不用电脑时跑；-Quick 是小规模。

产出：<ReportDir>\m2v-report.json 与 m2v-summary.txt（默认 target\m2v\<时间>\）。
退出码：0 全部通过；1 有门槛失败；3 无失败但有层被跳过或拒绝（门槛只算“部分通过”）。
#>
param(
    [string[]]$Gate = @('all'),
    [switch]$Quick,
    [switch]$Headless,
    [string]$DataRoot,
    [string]$ReportDir,
    [int]$Copies = 0,
    [int]$Kills = 0,
    [int]$Port = 9223
)
$ErrorActionPreference = 'Stop'
$Root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$Stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
if (-not $ReportDir) { $ReportDir = Join-Path $Root "target\m2v\$Stamp" }
$AutoDataRoot = -not $DataRoot
if ($AutoDataRoot) { $DataRoot = Join-Path ([IO.Path]::GetTempPath()) "nianlai-m2v-$Stamp" }
[IO.Directory]::CreateDirectory($ReportDir) | Out-Null
$Target = 'x86_64-pc-windows-msvc'
$AppExe = Join-Path $Root "target\$Target\release\nianlai.exe"
$StoreExe = Join-Path $Root "target\$Target\release\g0-store.exe"
if ($Copies -le 0) { $Copies = if ($Quick) { 60 } else { 10000 } }
if ($Kills -le 0) { $Kills = if ($Quick) { 3 } else { 30 } }

$Gates = @(1..6)
if (-not ($Gate -contains 'all')) { $Gates = @($Gate | ForEach-Object { [int]$_ } | Sort-Object -Unique) }
foreach ($g in $Gates) { if ($g -lt 1 -or $g -gt 6) { throw "unknown gate: $g" } }

$Names = @{
    1 = 'controlled copies, zero loss'
    2 = 'kill while writing, data consistent'
    3 = 'old databases upgrade'
    4 = 'backup export/import consistent'
    5 = 'trash chain: delete, restore, expire'
    6 = 'excluded/paused/sensitive leave no trace'
}
$Results = [ordered]@{}
foreach ($g in $Gates) { $Results["$g"] = [ordered]@{ gate = $g; name = $Names[$g]; layers = [ordered]@{} } }

function New-Layer([string]$Status, $Evidence, [string]$Reason = '') {
    [ordered]@{ status = $Status; reason = $Reason; evidence = $Evidence }
}
function Say([string]$Message) { Write-Host ("[m2v] " + $Message) }

# ------------------------------------------------------------------ 确定性层（cargo）

function Invoke-Cargo([string]$Label, [string[]]$CargoArgs, [switch]$NoTests) {
    $base = Join-Path $ReportDir ('cargo-' + ($Label -replace '[^A-Za-z0-9]+', '-'))
    Say ('cargo ' + ($CargoArgs -join ' '))
    $p = Start-Process -FilePath 'cargo' -ArgumentList $CargoArgs -WorkingDirectory $Root -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput "$base.out.log" -RedirectStandardError "$base.err.log"
    $exit = $p.ExitCode
    $text = (Get-Content -LiteralPath "$base.out.log" -Raw) + "`n" + (Get-Content -LiteralPath "$base.err.log" -Raw)
    $passed = 0; $failed = 0
    foreach ($m in [regex]::Matches($text, 'test result: \w+\. (\d+) passed; (\d+) failed')) {
        $passed += [int]$m.Groups[1].Value; $failed += [int]$m.Groups[2].Value
    }
    $tests = @([regex]::Matches($text, '(?m)^test (\S+) \.\.\. (ok|FAILED)') | ForEach-Object { $_.Groups[1].Value + '=' + $_.Groups[2].Value })
    $ok = ($exit -eq 0 -and $failed -eq 0 -and ($NoTests -or $passed -gt 0))
    [ordered]@{ ok = $ok; exit = $exit; passed = $passed; failed = $failed; tests = $tests; log = "$base.err.log" }
}

function Invoke-HeadlessGate([int]$Number) {
    $store = @('test', '-p', 'nianlai-store')
    $app = @('test', '-p', 'nianlai')
    $include = if ($Quick) { @() } else { @('--', '--include-ignored') }
    $runs = @(switch ($Number) {
        1 { , @{ label = 'g1'; cargo = $store + @('gates_tests::gate1_') + $include } }
        2 { , @{ label = 'g2'; cargo = $store + @('gates_tests::gate2_') + $include } }
        3 { , @{ label = 'g3'; cargo = $store + @('gates_tests::gate3_') } }
        4 { , @{ label = 'g4'; cargo = $store + @('gates_tests::gate4_') } }
        5 { @(@{ label = 'g5-store'; cargo = $store + @('gates_tests::gate5_') }, @{ label = 'g5-app'; cargo = $app + @('--lib', 'history::gate_tests::gate5_') }) }
        6 { , @{ label = 'g6-app'; cargo = $app + @('--lib', 'history::gate_tests::gate6_') } }
    })
    $evidence = [ordered]@{}
    $ok = $true
    foreach ($run in $runs) {
        $r = Invoke-Cargo $run.label $run.cargo
        $evidence[$run.label] = $r
        if (-not $r.ok) { $ok = $false }
    }
    if ($Number -eq 2) {
        # 复用 g0-store 的强杀写入进程（外部进程、真实 TerminateProcess），回执编号必须在库里。
        $crash = Invoke-G0Crash
        $evidence['g0_crash'] = $crash
        if (-not $crash.ok) { $ok = $false }
    }
    New-Layer $(if ($ok) { 'pass' } else { 'fail' }) $evidence
}

function Invoke-G0Crash {
    Ensure-StoreExe
    $rounds = if ($Quick) { 3 } else { 30 }
    $out = Join-Path $ReportDir 'g0-crash-results.json'
    $log = Join-Path $ReportDir 'g0-crash.log'
    # g0-crash-test 只结束自己启动的 writer 进程。
    $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSScriptRoot\g0-crash-test.ps1`"", '-Rounds', $rounds, '-Binary', "`"$StoreExe`"", '-Output', "`"$out`"")
    $p = Start-Process -FilePath 'powershell.exe' -ArgumentList $arguments -NoNewWindow -Wait -PassThru -RedirectStandardOutput $log -RedirectStandardError "$log.err"
    $okRounds = 0
    if (Test-Path -LiteralPath $out) {
        $j = Get-Content -LiteralPath $out -Raw | ConvertFrom-Json
        $okRounds = @($j.results | Where-Object { $_.ok }).Count
        # 全部通过就删掉它的临时库（每次约 150 MB）；失败时留着查。
        if ($okRounds -eq $rounds -and $j.data_directory -and ([string]$j.data_directory).Contains('nianlai-g0-crash-')) {
            Remove-Item -LiteralPath $j.data_directory -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
    [ordered]@{ ok = ($p.ExitCode -eq 0 -and $okRounds -eq $rounds); rounds = $rounds; rounds_ok = $okRounds; results = $out; log = $log }
}

# ------------------------------------------------------------------ 真实层：辅助

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class M2vClip {
    [DllImport("user32.dll", SetLastError = true)] static extern bool OpenClipboard(IntPtr h);
    [DllImport("user32.dll", SetLastError = true)] static extern bool CloseClipboard();
    [DllImport("user32.dll", SetLastError = true)] static extern bool EmptyClipboard();
    [DllImport("user32.dll", SetLastError = true)] static extern IntPtr SetClipboardData(uint f, IntPtr h);
    [DllImport("user32.dll", SetLastError = true)] static extern IntPtr GetClipboardData(uint f);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern uint RegisterClipboardFormat(string n);
    [DllImport("user32.dll")] static extern uint GetClipboardSequenceNumber();
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateWindowEx(uint ex, string cls, string name, uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("kernel32.dll")] static extern IntPtr GlobalAlloc(uint f, UIntPtr n);
    [DllImport("kernel32.dll")] static extern IntPtr GlobalLock(IntPtr h);
    [DllImport("kernel32.dll")] static extern bool GlobalUnlock(IntPtr h);
    static IntPtr owner = IntPtr.Zero;

    static bool Open() {
        if (owner == IntPtr.Zero) owner = CreateWindowEx(0, "STATIC", "", 0, 0, 0, 0, 0, new IntPtr(-3), IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        for (int i = 0; i < 100; i++) { if (OpenClipboard(owner)) return true; Thread.Sleep(20); }
        return false;
    }
    static IntPtr Global(byte[] bytes) {
        IntPtr h = GlobalAlloc(2, new UIntPtr((uint)Math.Max(bytes.Length, 1)));
        IntPtr p = GlobalLock(h);
        Marshal.Copy(bytes, 0, p, bytes.Length);
        GlobalUnlock(h);
        return h;
    }
    public static uint Sequence() { return GetClipboardSequenceNumber(); }
    // 写入纯文本；marker 不为空时再带一个“不要记录”标记格式（4 字节 DWORD）。返回写完后的剪贴板序列号。
    public static uint SetText(string text, string marker, uint dword) {
        if (!Open()) throw new Exception("open_clipboard_failed");
        try {
            EmptyClipboard();
            if (text != null) {
                SetClipboardData(13, Global(Encoding.Unicode.GetBytes(text + "\0")));
                if (marker != null) {
                    uint f = RegisterClipboardFormat(marker);
                    SetClipboardData(f, Global(BitConverter.GetBytes(dword)));
                }
            }
        } finally { CloseClipboard(); }
        return GetClipboardSequenceNumber();
    }
    public static string GetText() {
        if (!Open()) return null;
        try {
            IntPtr h = GetClipboardData(13);
            if (h == IntPtr.Zero) return null;
            IntPtr p = GlobalLock(h);
            try { return Marshal.PtrToStringUni(p); } finally { GlobalUnlock(h); }
        } finally { CloseClipboard(); }
    }
}

public static class M2vScan {
    static int IndexOf(byte[] hay, byte[] needle) {
        if (needle.Length == 0 || hay.Length < needle.Length) return -1;
        int last = hay.Length - needle.Length;
        for (int i = 0; i <= last; i++) {
            if (hay[i] != needle[0]) continue;
            int j = 1;
            while (j < needle.Length && hay[i + j] == needle[j]) j++;
            if (j == needle.Length) return i;
        }
        return -1;
    }
    // 目录下所有文件（含 -wal、-shm）里按原始字节找每个关键词的 UTF-8 与 UTF-16LE 形式。
    // 返回 "文件|关键词|utf8" 之类的命中行；读不了的文件返回 "!unreadable|文件"。
    public static string[] Find(string dir, string[] needles) {
        var hits = new List<string>();
        foreach (string file in Directory.EnumerateFiles(dir, "*", SearchOption.AllDirectories)) {
            byte[] bytes;
            try {
                using (var fs = new FileStream(file, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete)) {
                    bytes = new byte[fs.Length];
                    int got = 0;
                    while (got < bytes.Length) { int n = fs.Read(bytes, got, bytes.Length - got); if (n <= 0) break; got += n; }
                    if (got < bytes.Length) Array.Resize(ref bytes, got);
                }
            } catch { hits.Add("!unreadable|" + file); continue; }
            foreach (string needle in needles) {
                if (IndexOf(bytes, Encoding.UTF8.GetBytes(needle)) >= 0) hits.Add(file + "|" + needle + "|utf8");
                if (IndexOf(bytes, Encoding.Unicode.GetBytes(needle)) >= 0) hits.Add(file + "|" + needle + "|utf16");
            }
        }
        return hits.ToArray();
    }
}
'@

function Get-NianlaiProcesses { @(Get-CimInstance Win32_Process -Filter "Name = 'nianlai.exe'") }

# 真实层的准入检查。返回 $null 表示可以开始，否则返回拒绝原因。
function Test-RealLayerAllowed {
    if ($Headless) { return 'headless mode (-Headless)' }
    if (-not (Test-Path -LiteralPath $AppExe)) { return "release exe not found: $AppExe (build it first / 先构建样机)" }
    $running = Get-NianlaiProcesses
    if ($running.Count -gt 0) {
        $daily = @($running | Where-Object { $_.ExecutablePath -and $_.ExecutablePath -notlike "*$Target*" })
        $list = ($running | ForEach-Object { "pid=$($_.ProcessId) $($_.ExecutablePath)" }) -join '; '
        $why = if ($daily.Count -gt 0) { 'the daily instance is running from target\release' } else { 'another nianlai.exe is running' }
        Write-Host "[m2v] REFUSED: $why ($list)." -ForegroundColor Yellow
        Write-Host '[m2v] 拒绝启动真实应用层：检测到正在运行的拈来。单实例插件会把新实例转发给它，脚本不会碰它。' -ForegroundColor Yellow
        Write-Host '[m2v] 请先自行退出它再跑真实层；或加 -Headless 只跑确定性层。' -ForegroundColor Yellow
        return "refused: nianlai.exe already running ($list)"
    }
    if (-not (Get-Command node -ErrorAction SilentlyContinue)) { return 'node not found (needed for CDP pause)' }
    return $null
}

# 样机的隔离环境变量：设置后启动，启动完立即还原本脚本进程的环境。
function Start-App([string]$Base, [string]$SettingsJson) {
    $data = Join-Path $Base 'data'
    $appdata = Join-Path $Base 'appdata'
    $settingsDir = Join-Path $appdata 'Nianlai'
    [IO.Directory]::CreateDirectory($data) | Out-Null
    [IO.Directory]::CreateDirectory($settingsDir) | Out-Null
    if ($SettingsJson) { [IO.File]::WriteAllText((Join-Path $settingsDir 'settings.json'), $SettingsJson, (New-Object Text.UTF8Encoding($false))) }
    $vars = [ordered]@{
        NIANLAI_G0_DATA_DIR = $data; APPDATA = $appdata; LOCALAPPDATA = (Join-Path $Base 'local')
        WEBVIEW2_USER_DATA_FOLDER = (Join-Path $Base 'webview'); NIANLAI_GUARD = '0'; NIANLAI_G0_SAMPLES = '0'
        NIANLAI_G0_AUTOCYCLE = $null; NIANLAI_G0_RESUSPEND_MIN = $null
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$Port"
    }
    $old = @{}
    foreach ($k in $vars.Keys) { $old[$k] = [Environment]::GetEnvironmentVariable($k, 'Process'); [Environment]::SetEnvironmentVariable($k, $vars[$k], 'Process') }
    try {
        $dir = Split-Path -Parent $AppExe
        $p = Start-Process -FilePath $AppExe -WorkingDirectory $dir -PassThru
    } finally {
        foreach ($k in $vars.Keys) { [Environment]::SetEnvironmentVariable($k, $old[$k], 'Process') }
    }
    $p
}

# 只结束本脚本启动的样机：先核对 PID 的路径确实是 $AppExe 且含 x86_64-pc-windows-msvc。
function Stop-App($Process) {
    if (-not $Process) { return }
    $info = Get-CimInstance Win32_Process -Filter ("ProcessId = " + $Process.Id) -ErrorAction SilentlyContinue
    if (-not $info) { return }
    if (-not $info.ExecutablePath -or $info.ExecutablePath -notlike "*$Target*" -or $info.ExecutablePath -ne $AppExe) {
        throw ("refuse to stop pid $($Process.Id): unexpected path " + $info.ExecutablePath)
    }
    Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
    try { $Process.WaitForExit(15000) | Out-Null } catch { }
}

function New-LogTail([string]$Path) {
    $pos = 0L
    if (Test-Path -LiteralPath $Path) { $pos = (Get-Item -LiteralPath $Path).Length }
    [pscustomobject]@{ Path = $Path; Pos = $pos; Rest = '' }
}
function Read-LogTail($Tail) {
    if (-not (Test-Path -LiteralPath $Tail.Path)) { return @() }
    $fs = [IO.File]::Open($Tail.Path, 'Open', 'Read', 'ReadWrite, Delete')
    try {
        if ($fs.Length -lt $Tail.Pos) { $Tail.Pos = 0L; $Tail.Rest = '' }
        $count = [int]($fs.Length - $Tail.Pos)
        if ($count -le 0) { return @() }
        $fs.Position = $Tail.Pos
        $buffer = New-Object byte[] $count
        $got = 0
        while ($got -lt $count) { $n = $fs.Read($buffer, $got, $count - $got); if ($n -le 0) { break }; $got += $n }
        $Tail.Pos += $got
        $text = $Tail.Rest + [Text.Encoding]::UTF8.GetString($buffer, 0, $got)
    } finally { $fs.Dispose() }
    $lines = $text -split "`n"
    $Tail.Rest = $lines[$lines.Count - 1]
    $events = New-Object Collections.Generic.List[object]
    for ($i = 0; $i -lt $lines.Count - 1; $i++) {
        if ($lines[$i].Trim()) { try { $events.Add(($lines[$i] | ConvertFrom-Json)) } catch { } }
    }
    , $events.ToArray()
}

# 等到出现满足条件的日志事件；返回事件或 $null。同时把读到的全部事件追加到 $All（供事后查错误）。
function Wait-LogEvent($Tail, [scriptblock]$Match, [int]$TimeoutMs, $All = $null) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        foreach ($e in (Read-LogTail $Tail)) {
            if ($null -ne $All) { $All.Add($e) }
            if (& $Match $e) { return $e }
        }
        Start-Sleep -Milliseconds 8
    } while ($clock.ElapsedMilliseconds -lt $TimeoutMs)
    $null
}

function Wait-AppReady($Process, $Tail, $All) {
    $e = Wait-LogEvent $Tail { param($e) $e.event -eq 'store_opened' } 60000 $All
    if (-not $e) { throw 'app did not log store_opened within 60s' }
    Start-Sleep -Milliseconds 1500
    if ($Process.HasExited) { throw 'app exited right after start' }
    $e
}

function Find-AppErrors($Events) {
    @($Events | Where-Object { ($_.event -match '(error|failed)$') -or ($_.event -eq 'store_check' -and -not $_.success) } | ForEach-Object { $_.event })
}

# 一次复制并等应用给出这次的结论（按剪贴板序列号对应）。返回 capture 事件或 $null。
function Copy-AndWait($Tail, [string]$Text, $Marker = $null, [uint32]$Dword = 0, [int]$TimeoutMs = 6000, $All = $null) {
    $sequence = [M2vClip]::SetText($Text, $Marker, $Dword)
    Wait-LogEvent $Tail { param($e) $e.event -eq 'capture' -and $null -ne $e.sequence -and [uint32]$e.sequence -ge $sequence } $TimeoutMs $All
}

function Get-StoreJson([string]$Command, [string[]]$More = @()) {
    $out = & $StoreExe $Command --dir (Join-Path $script:Base 'data') @More 2>&1
    if ($LASTEXITCODE -ne 0) { return [pscustomobject]@{ ok = $false; error = ($out -join ' ') } }
    $j = ($out | Where-Object { $_ -is [string] } | Select-Object -Last 1) | ConvertFrom-Json
    $j | Add-Member -NotePropertyName ok -NotePropertyValue $true -Force
    $j
}
function Ensure-StoreExe {
    # 每次运行构建一次（增量，很快），免得用到缺少 texts 命令的旧版本。
    if ($script:StoreBuilt) { return }
    $b = Invoke-Cargo 'g0-store-build' @('build', '--release', '--target', $Target, '-p', 'g0-store') -NoTests
    if (-not $b.ok -or -not (Test-Path -LiteralPath $StoreExe)) { throw "g0-store build failed: $($b.log)" }
    $script:StoreBuilt = $true
}

# 比较“期望文字集合”和库里的实际集合。
function Compare-Texts($Expected, $Stored) {
    $count = @{}
    foreach ($t in $Stored) { if ($count.ContainsKey($t)) { $count[$t]++ } else { $count[$t] = 1 } }
    $missing = @($Expected | Where-Object { -not $count.ContainsKey($_) })
    $duplicated = @($count.Keys | Where-Object { $count[$_] -gt 1 })
    $expectedSet = @{}
    foreach ($t in $Expected) { $expectedSet[$t] = $true }
    $unexpected = @($count.Keys | Where-Object { -not $expectedSet.ContainsKey($_) })
    [ordered]@{ expected = @($Expected).Count; stored = @($Stored).Count; missing = $missing.Count; missing_sample = @($missing | Select-Object -First 10)
        duplicated = $duplicated.Count; duplicated_sample = @($duplicated | Select-Object -First 10); unexpected = $unexpected.Count; unexpected_sample = @($unexpected | Select-Object -First 10) }
}

# 剪贴板内容在整个真实层前后保存、还原（只能还原纯文本）。
$script:SavedClipboard = $null
$script:ClipboardSaved = $false

# ------------------------------------------------------------------ 真实层：门槛 1

function Invoke-RealGate1 {
    Ensure-StoreExe
    $script:Base = Join-Path $DataRoot 'g1'
    $run = Get-Date -Format 'HHmmss'
    $prefix = "m2v-g1-$run-"
    $tail = New-LogTail (Join-Path $script:Base 'data\panel-log.jsonl')
    $all = New-Object Collections.Generic.List[object]
    # transient 关掉：每次复制立即提交，回执才对得上；这里测的是不丢，不是临时内容丢弃。
    $app = Start-App $script:Base '{"capture":{"transient":false}}'
    try {
        $null = Wait-AppReady $app $tail $all
        $expected = New-Object Collections.Generic.List[string]
        $noAck = New-Object Collections.Generic.List[string]
        $duplicates = 0
        $clock = [Diagnostics.Stopwatch]::StartNew()
        for ($i = 1; $i -le $Copies; $i++) {
            $text = $prefix + $i.ToString('D6')
            $e = Copy-AndWait $tail $text -All $all
            $expected.Add($text)
            if (-not ($e -and $null -ne $e.record_id)) { $noAck.Add($text) }
            if ($i % 10 -eq 0) {
                # 再复制一条更早的：只累加次数，库里仍是一份。
                $old = $prefix + ([int]($i / 2)).ToString('D6')
                $e2 = Copy-AndWait $tail $old -All $all
                if ($e2 -and $e2.outcome -eq 'duplicate') { $duplicates++ }
            }
            if ($i % 500 -eq 0) { Say "gate 1: $i / $Copies copies, $([int]$clock.Elapsed.TotalSeconds)s" }
        }
        Start-Sleep -Seconds 3
        $texts = Get-StoreJson 'texts' @('--prefix', $prefix)
        $check = Get-StoreJson 'check'
        $errors = Find-AppErrors $all
        if (-not $texts.ok) { return New-Layer 'fail' ([ordered]@{ error = $texts.error }) 'texts command failed' }
        $diff = Compare-Texts $expected $texts.texts
        $ok = ($diff.missing -eq 0 -and $diff.duplicated -eq 0 -and $diff.unexpected -eq 0 -and $check.ok -and $errors.Count -eq 0)
        New-Layer $(if ($ok) { 'pass' } else { 'fail' }) ([ordered]@{
            copies = $Copies; no_ack_within_6s = $noAck.Count; no_ack_sample = @($noAck | Select-Object -First 10)
            recopy_reported_duplicate = $duplicates; compare = $diff; integrity_ok = $check.ok; app_errors = $errors
            seconds = [int]$clock.Elapsed.TotalSeconds; data_dir = (Join-Path $script:Base 'data') })
    } finally { Stop-App $app }
}

# ------------------------------------------------------------------ 真实层：门槛 2

function Invoke-RealGate2 {
    Ensure-StoreExe
    $script:Base = Join-Path $DataRoot 'g2'
    $run = Get-Date -Format 'HHmmss'
    $prefix = "m2v-g2-$run-"
    $tail = New-LogTail (Join-Path $script:Base 'data\panel-log.jsonl')
    $random = New-Object Random 20261109
    $acked = New-Object Collections.Generic.List[string]
    $rounds = New-Object Collections.Generic.List[object]
    $ok = $true
    for ($round = 1; $round -le $Kills; $round++) {
        $all = New-Object Collections.Generic.List[object]
        $app = Start-App $script:Base '{"capture":{"transient":false}}'
        $row = [ordered]@{ round = $round }
        try {
            # 干净启动：能打开库、没有错误事件、进程还活着。
            $null = Wait-AppReady $app $tail $all
            $row.start_errors = Find-AppErrors $all
            $burst = $random.Next(5, 25)
            for ($n = 1; $n -le $burst; $n++) {
                $text = "$prefix$('{0:D2}' -f $round)-$('{0:D3}' -f $n)"
                $e = Copy-AndWait $tail $text -All $all
                if ($e -and $null -ne $e.record_id) { $acked.Add($text) }
            }
            # 最后一条写完剪贴板立刻强杀（随机 0–40ms 后），落点在采集和提交中途；这一条不要求保留。
            $lastText = "$prefix$('{0:D2}' -f $round)-last"
            $lastSequence = [M2vClip]::SetText($lastText, $null, 0)
            Start-Sleep -Milliseconds $random.Next(0, 40)
        } finally { Stop-App $app }
        # 回执只认日志里带 record_id 的 capture 事件（日志在提交之后才写）；最后一条若恰好已记入日志也算。
        foreach ($e in (Read-LogTail $tail)) {
            $all.Add($e)
            if ($e.event -eq 'capture' -and $null -ne $e.record_id -and $null -ne $e.sequence -and [uint32]$e.sequence -ge $lastSequence) { $acked.Add($lastText) }
        }
        $texts = Get-StoreJson 'texts' @('--prefix', $prefix)
        $check = Get-StoreJson 'check'
        $diff = if ($texts.ok) { Compare-Texts $acked $texts.texts } else { $null }
        $roundOk = ($texts.ok -and $check.ok -and $diff.missing -eq 0 -and $diff.duplicated -eq 0 -and $row.start_errors.Count -eq 0)
        $row.acked_total = $acked.Count; $row.integrity_ok = $check.ok
        $row.missing = if ($diff) { $diff.missing } else { -1 }; $row.missing_sample = if ($diff) { $diff.missing_sample } else { @() }
        $row.duplicated = if ($diff) { $diff.duplicated } else { -1 }; $row.ok = $roundOk
        $rounds.Add($row)
        if (-not $roundOk) { $ok = $false; Say "gate 2 round $round FAILED"; break }
        Say "gate 2: round $round / $Kills ok (acked so far $($acked.Count))"
    }
    New-Layer $(if ($ok) { 'pass' } else { 'fail' }) ([ordered]@{ kills = $Kills; completed_rounds = $rounds.Count; acked_total = $acked.Count; rounds = $rounds; data_dir = (Join-Path $script:Base 'data') })
}

# ------------------------------------------------------------------ 真实层：门槛 6

# PR #23 的假密码管理器：把测试程序自己复制成 KeePass.exe 之类的文件名，以辅助模式启动，由它写剪贴板。
function Get-FakePasswordManagerKit {
    $log = Join-Path $ReportDir 'cargo-g6-test-build.log'
    $null = Start-Process -FilePath 'cargo' -ArgumentList @('test', '-p', 'nianlai', '--lib', '--no-run', '--message-format=json') -WorkingDirectory $Root -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err"
    $exe = $null
    foreach ($line in (Get-Content -LiteralPath $log)) {
        if ($line.StartsWith('{') -and $line -match '"executable":"') {
            try { $j = $line | ConvertFrom-Json } catch { continue }
            if ($j.executable -and $j.target.name -eq 'nianlai_lib' -and $j.profile.test) { $exe = $j.executable }
        }
    }
    if (-not $exe -or -not (Test-Path -LiteralPath $exe)) { throw "cannot locate the nianlai unit-test binary (see $log)" }
    $name = & $exe --list 2>$null | Where-Object { $_ -match 'fake_password_manager_helper: test$' } | Select-Object -First 1
    if (-not $name) { throw 'fake password manager helper test not found' }
    [pscustomobject]@{ Exe = $exe; Test = ($name -replace ': test$', '') }
}

# 让名为 $ExeName 的假进程写入剪贴板；返回这次写入后的序列号和进程对象（调用方用 Stop-Fake 收尾）。
function Start-Fake($Kit, [string]$ExeName, [string]$Text) {
    $dir = Join-Path $DataRoot ("fake-" + [Guid]::NewGuid().ToString('N'))
    $bin = Join-Path $dir 'bin'
    [IO.Directory]::CreateDirectory($bin) | Out-Null
    $copy = Join-Path $bin $ExeName
    try { New-Item -ItemType HardLink -Path $copy -Target $Kit.Exe | Out-Null } catch { Copy-Item -LiteralPath $Kit.Exe -Destination $copy }
    $vars = @{ NIANLAI_FAKE_PM_DIR = $dir; NIANLAI_FAKE_PM_TEXT = $Text; NIANLAI_FAKE_PM_MARKER = $null; NIANLAI_FAKE_PM_DWORD = $null }
    $old = @{}
    foreach ($k in $vars.Keys) { $old[$k] = [Environment]::GetEnvironmentVariable($k, 'Process'); [Environment]::SetEnvironmentVariable($k, $vars[$k], 'Process') }
    try {
        $p = Start-Process -FilePath $copy -ArgumentList @($Kit.Test, '--exact', '--nocapture', '--test-threads=1') -WindowStyle Hidden -PassThru
    } finally { foreach ($k in $vars.Keys) { [Environment]::SetEnvironmentVariable($k, $old[$k], 'Process') } }
    $ready = Join-Path $dir 'ready'
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path -LiteralPath $ready) -and $clock.ElapsedMilliseconds -lt 15000) { Start-Sleep -Milliseconds 30 }
    if (-not (Test-Path -LiteralPath $ready) -or (Get-Content -LiteralPath $ready -Raw) -ne 'ok') { return [pscustomobject]@{ Ok = $false; Dir = $dir; Process = $p; Sequence = 0 } }
    [pscustomobject]@{ Ok = $true; Dir = $dir; Process = $p; Sequence = [M2vClip]::Sequence() }
}
function Stop-Fake($Fake) {
    # 假进程看到 stop 文件会自己退出（最迟 30 秒自行结束）；不对它调用 Stop-Process。
    [IO.File]::WriteAllText((Join-Path $Fake.Dir 'stop'), '')
    try { $Fake.Process.WaitForExit(6000) | Out-Null } catch { }
}

function Invoke-CdpPause([bool]$Paused) {
    $env:M2V_EXPR = "window.__TAURI_INTERNALS__.invoke('set_capture_paused',{paused:$($Paused.ToString().ToLower())}).then(()=>window.__TAURI_INTERNALS__.invoke('get_capture_state')).then(s=>s.paused)"
    try { $out = & node (Join-Path $PSScriptRoot 'm2v-cdp.mjs') $Port } finally { Remove-Item Env:\M2V_EXPR -ErrorAction SilentlyContinue }
    try { $j = ($out | Select-Object -Last 1) | ConvertFrom-Json } catch { return $false }
    [bool]($j.ok -and $j.value -eq $Paused)
}

function Invoke-RealGate6 {
    Ensure-StoreExe
    $kit = Get-FakePasswordManagerKit
    $script:Base = Join-Path $DataRoot 'g6'
    $tag = "m2vtrace" + (Get-Date -Format 'HHmmss')
    $tail = New-LogTail (Join-Path $script:Base 'data\panel-log.jsonl')
    $all = New-Object Collections.Generic.List[object]
    # transient 关掉：对照组立即提交、带记录编号回执；这里测的是被挡的不留痕，不是临时内容。
    $app = Start-App $script:Base '{"capture":{"transient":false}}'
    $samples = New-Object Collections.Generic.List[object]
    $ok = $true
    try {
        $null = Wait-AppReady $app $tail $all
        $body = { param($kind) "$tag$kind 拈来验收样本正文" }
        $addSample = {
            param($kind, $want, $event)
            $got = if ($event) { if ($event.reason) { [string]$event.reason } else { [string]$event.outcome } } else { 'no_event' }
            $row = [ordered]@{ kind = $kind; key = "$tag$kind"; expected = $want; observed = $got; pass = ($got -eq $want) }
            $samples.Add($row)
            if (-not $row.pass) { Say "gate 6 sample ${kind}: expected $want, observed $got" }
        }
        # 1) 排除应用：默认名单里的假进程（假 KeePass.exe 等，来自 PR #23），含大小写差异。
        $names = @('KeePass.exe', 'KeePassXC.exe', 'Bitwarden.exe', '1Password.exe', 'Enpass.exe', 'RoboForm.exe', 'keepassxc.EXE')
        foreach ($name in $names) {
            $kind = 'excluded-' + [array]::IndexOf($names, $name)
            $fake = Start-Fake $kit $name (& $body $kind)
            try {
                if (-not $fake.Ok) { & $addSample $kind 'excluded_app' $null; continue }
                $e = Wait-LogEvent $tail { param($e) $e.event -eq 'capture' -and $null -ne $e.sequence -and [uint32]$e.sequence -ge $fake.Sequence -and $e.reason -ne 'sequence_changed' } 8000 $all
                & $addSample $kind 'excluded_app' $e
            } finally { Stop-Fake $fake }
        }
        # 2) 四种“不要记录”标记（普通进程 + 标记格式；DWORD 型标记值为 0 才算不要记录）。
        foreach ($marker in @('Clipboard Viewer Ignore', 'ExcludeClipboardContentFromMonitorProcessing', 'CanIncludeInClipboardHistory', 'CanUploadToCloudClipboard')) {
            $kind = 'marker-' + ($marker -replace '[^A-Za-z]', '').ToLower().Substring(0, 12)
            $e = Copy-AndWait $tail (& $body $kind) $marker 0 8000 $all
            & $addSample $kind 'marker' $e
        }
        # 3) 敏感内容（GitHub Token 形态）：规则链第二层。
        $token = 'ghp_' + 'aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0eF3hJ6'
        $e = Copy-AndWait $tail ((& $body 'sensitive') + ' ' + $token) $null 0 8000 $all
        & $addSample 'sensitive' 'sensitive' $e
        # 4) 暂停期间（通过样机的调试端口调用 set_capture_paused；不发送按键）。
        if (Invoke-CdpPause $true) {
            try {
                $e = Copy-AndWait $tail (& $body 'paused') $null 0 8000 $all
                & $addSample 'paused' 'paused' $e
            } finally { [void](Invoke-CdpPause $false) }
        } else {
            $samples.Add([ordered]@{ kind = 'paused'; key = "${tag}paused"; expected = 'paused'; observed = 'cdp_unavailable'; pass = $false })
            Say 'gate 6: could not pause through CDP'
        }
        # 5) 对照组：不该被挡，必须记下，证明扫描和流程是有效的。
        $e = Copy-AndWait $tail (& $body 'control') $null 0 8000 $all
        $row = [ordered]@{ kind = 'control'; key = "${tag}control"; expected = 'recorded'; observed = $(if ($e -and $null -ne $e.record_id) { 'recorded' } else { 'not_recorded' }); pass = [bool]($e -and $null -ne $e.record_id) }
        $samples.Add($row)
        Start-Sleep -Seconds 3

        # 运行中扫描一遍（WAL、SHM 都还活着），结束样机后再扫一遍。
        $dataDir = Join-Path $script:Base 'data'
        $blocked = @($samples | Where-Object { $_.kind -ne 'control' } | ForEach-Object { $_.key })
        $control = "${tag}control"
        $liveHits = [M2vScan]::Find($dataDir, [string[]]($blocked + $control))
        $dbText = Get-StoreJson 'texts' @('--prefix', $tag)
    } finally { Stop-App $app }
    $dataDir = Join-Path $script:Base 'data'
    $afterHits = [M2vScan]::Find($dataDir, [string[]]($blocked + $control))
    $bad = @(($liveHits + $afterHits) | Where-Object { $_ -notlike "*|$control|*" })
    $controlHits = @(($liveHits + $afterHits) | Where-Object { $_ -like "*|$control|*" -and $_ -like '*history.db*' })
    $unreadable = @($bad | Where-Object { $_ -like '!unreadable*' })
    $leaks = @($bad | Where-Object { $_ -notlike '!unreadable*' })
    $dbBlocked = if ($dbText.ok) { @($dbText.texts | Where-Object { $t = $_; $blocked | Where-Object { $t -like "*$_*" } }) } else { @('<texts command failed>') }
    $files = @(Get-ChildItem -LiteralPath $dataDir -Recurse -File | ForEach-Object { $_.FullName.Substring($dataDir.Length + 1) + ' (' + $_.Length + ')' })
    $failedSamples = @($samples | Where-Object { -not $_.pass })
    $ok = ($failedSamples.Count -eq 0 -and $leaks.Count -eq 0 -and $unreadable.Count -eq 0 -and $dbBlocked.Count -eq 0 -and $controlHits.Count -gt 0)
    New-Layer $(if ($ok) { 'pass' } else { 'fail' }) ([ordered]@{
        samples = $samples; file_hits_for_blocked_text = $leaks; unreadable_files = $unreadable; db_rows_with_blocked_text = $dbBlocked
        control_found_in_db_files = $controlHits.Count; scanned_files = $files; data_dir = $dataDir })
}

# ------------------------------------------------------------------ 主流程

function Invoke-RealGate([int]$Number) {
    switch ($Number) {
        1 { Invoke-RealGate1 }
        2 { Invoke-RealGate2 }
        6 { Invoke-RealGate6 }
    }
}

$started = Get-Date
$denied = $null
$realWanted = @($Gates | Where-Object { $_ -in 1, 2, 6 })
if ($realWanted.Count -gt 0) { $denied = Test-RealLayerAllowed }
Say ("gates: " + ($Gates -join ',') + $(if ($Quick) { ' (quick)' } else { '' }) + "; report dir: $ReportDir")
if ($realWanted.Count -gt 0 -and -not $denied) { Say "real layers on isolated data under $DataRoot (uses your clipboard; restored afterwards if it held plain text)" }

try {
    if ($realWanted.Count -gt 0 -and -not $denied) {
        $script:SavedClipboard = [M2vClip]::GetText(); $script:ClipboardSaved = $true
    }
    foreach ($g in $Gates) {
        $r = $Results["$g"]
        try { $r.layers['headless'] = Invoke-HeadlessGate $g } catch { $r.layers['headless'] = New-Layer 'fail' ([ordered]@{ error = "$_" }) "exception" }
        Say ("gate $g headless: " + $r.layers['headless'].status)
        if ($g -in 1, 2, 6) {
            if ($denied) { $r.layers['real'] = New-Layer 'skipped' ([ordered]@{}) $denied }
            else {
                try { $r.layers['real'] = Invoke-RealGate $g } catch { $r.layers['real'] = New-Layer 'fail' ([ordered]@{ error = "$_" }) 'exception' }
            }
            Say ("gate $g real: " + $r.layers['real'].status + $(if ($r.layers['real'].reason) { ' (' + $r.layers['real'].reason + ')' } else { '' }))
        }
    }
} finally {
    if ($script:ClipboardSaved) { try { [void][M2vClip]::SetText($script:SavedClipboard, $null, 0) } catch { } }
}

# 汇总
$anyFail = $false; $anyPartial = $false
foreach ($r in $Results.Values) {
    $statuses = @($r.layers.Values | ForEach-Object { $_.status })
    $r.status = if ($statuses -contains 'fail') { 'fail' } elseif ($statuses -contains 'skipped') { 'partial' } else { 'pass' }
    if ($r.status -eq 'fail') { $anyFail = $true }
    if ($r.status -eq 'partial') { $anyPartial = $true }
}
$report = [ordered]@{
    started = $started.ToString('s'); finished = (Get-Date).ToString('s'); quick = [bool]$Quick; copies = $Copies; kills = $Kills
    data_root = $DataRoot; app_exe = $AppExe; gates = @($Results.Values)
}
$jsonPath = Join-Path $ReportDir 'm2v-report.json'
$report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $jsonPath -Encoding utf8
$lines = New-Object Collections.Generic.List[string]
$lines.Add("M2-V gates  $($started.ToString('s'))  quick=$([bool]$Quick)  copies=$Copies  kills=$Kills")
foreach ($r in $Results.Values) {
    $lines.Add(("gate {0}  {1,-8} {2}" -f $r.gate, $r.status.ToUpper(), $r.name))
    foreach ($name in $r.layers.Keys) {
        $l = $r.layers[$name]
        $detail = ''
        if ($l.reason) { $detail = " - $($l.reason)" }
        elseif ($l.evidence -and $l.evidence.Contains('compare')) { $c = $l.evidence.compare; $detail = " - expected=$($c.expected) stored=$($c.stored) missing=$($c.missing) duplicated=$($c.duplicated)" }
        elseif ($l.evidence -and $l.evidence.Contains('rounds')) { $detail = " - rounds=$($l.evidence.completed_rounds)/$($l.evidence.kills) acked=$($l.evidence.acked_total)" }
        else {
            $tests = 0; foreach ($v in $l.evidence.Values) { if ($v -is [System.Collections.IDictionary] -and $v.Contains('passed')) { $tests += $v.passed } }
            if ($tests -gt 0) { $detail = " - tests passed=$tests" }
        }
        $lines.Add(("    {0,-9} {1}{2}" -f $name, $l.status, $detail))
    }
}
$lines.Add("report: $jsonPath")
$summaryPath = Join-Path $ReportDir 'm2v-summary.txt'
$lines | Set-Content -LiteralPath $summaryPath -Encoding utf8
$lines | ForEach-Object { Write-Host $_ }
# 自动生成的隔离数据：没有失败就删掉；有失败留着查，路径在报告里。
if ($AutoDataRoot -and -not $anyFail -and (Test-Path -LiteralPath $DataRoot)) {
    Remove-Item -LiteralPath $DataRoot -Recurse -Force -ErrorAction SilentlyContinue
}
if ($anyFail) { exit 1 }
if ($anyPartial) { exit 3 }
exit 0
