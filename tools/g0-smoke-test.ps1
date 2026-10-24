param(
    [string]$DataDirectory,
    [string]$Binary = (Join-Path $PSScriptRoot '../target/release/nianlai.exe'),
    [string]$StoreBinary = (Join-Path $PSScriptRoot '../target/release/g0-store.exe'),
    [switch]$LiveSnapshot,
    [switch]$Samples,
    [ValidateRange(10, 3600)][int]$Seconds = 10,
    [string]$Output = (Join-Path $PSScriptRoot '../target/g0-store/smoke-results.json')
)
$ErrorActionPreference = 'Stop'
$Binary = (Resolve-Path -LiteralPath $Binary).Path
$existing = @(Get-CimInstance Win32_Process -Filter "Name = 'nianlai.exe'")
if ($existing.Count -gt 0) {
    $ids = ($existing | ForEach-Object { $_.ProcessId }) -join ','
    throw ('another_prototype_is_running; not_started_or_stopped; pids=' + $ids)
}
if (-not $DataDirectory) { $DataDirectory = Join-Path ([IO.Path]::GetTempPath()) ('nianlai-g0-smoke-' + [Guid]::NewGuid().ToString('N')) }
$DataDirectory = [IO.Path]::GetFullPath($DataDirectory)
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
if (-not $DataDirectory.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'smoke_requires_temporary_directory' }
[IO.Directory]::CreateDirectory($DataDirectory) | Out-Null
$Output = [IO.Path]::GetFullPath($Output)
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Output)) | Out-Null
$names = @('NIANLAI_G0_DATA_DIR','LOCALAPPDATA','WEBVIEW2_USER_DATA_FOLDER','NIANLAI_G0_SAMPLES','NIANLAI_G0_AUTOCYCLE')
$previous = @{}
foreach ($name in $names) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
$env:NIANLAI_G0_DATA_DIR = $DataDirectory
$env:LOCALAPPDATA = Join-Path $DataDirectory 'appdata'
$env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $DataDirectory 'webview'
$env:NIANLAI_G0_SAMPLES = if ($Samples) { '1' } else { '0' }
$env:NIANLAI_G0_AUTOCYCLE = $null
$owned = @{}
$samplesTaken = [Collections.Generic.List[object]]::new()
$log = Join-Path $DataDirectory 'panel-log.jsonl'
$startLines = if (Test-Path -LiteralPath $log) { @(Get-Content -LiteralPath $log).Count } else { 0 }
$app = $null
function Update-OwnedProcesses($all) {
    $byId = @{}
    foreach ($process in $all) { $byId[[int]$process.ProcessId] = $process }
    do {
        $added = $false
        foreach ($process in $all) {
            $parentId = [int]$process.ParentProcessId
            if ($owned.ContainsKey($parentId) -and $byId.ContainsKey($parentId) -and $byId[$parentId].CreationDate -eq $owned[$parentId] -and -not $owned.ContainsKey([int]$process.ProcessId) -and $process.CreationDate -ge $owned[$parentId]) {
                $owned[[int]$process.ProcessId] = $process.CreationDate
                $added = $true
            }
        }
    } while ($added)
}
try {
    $app = Start-Process -FilePath $Binary -PassThru -WindowStyle Hidden
    $rootProcess = Get-CimInstance Win32_Process -Filter ('ProcessId = ' + $app.Id)
    $owned[[int]$app.Id] = $rootProcess.CreationDate
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt $Seconds) {
        if ($app.HasExited) { throw 'prototype_exited_early' }
        $all = @(Get-CimInstance Win32_Process)
        Update-OwnedProcesses $all
        $perf = @(Get-CimInstance Win32_PerfFormattedData_PerfProc_Process | Where-Object { $owned.ContainsKey([int]$_.IDProcess) })
        $samplesTaken.Add([ordered]@{ seconds=$timer.Elapsed.TotalSeconds; processes=$perf.Count; private_working_set_bytes=($perf | Measure-Object WorkingSetPrivate -Sum).Sum; working_set_bytes=($perf | Measure-Object WorkingSet -Sum).Sum; private_bytes=($perf | Measure-Object PrivateBytes -Sum).Sum })
        Start-Sleep -Milliseconds 250
    }
    $live = $null
    if ($LiveSnapshot) {
        $live = & $StoreBinary snapshot --dir $DataDirectory | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0) { throw 'live_snapshot_failed' }
    }
    $rows = @(Get-Content -LiteralPath $log | Select-Object -Skip $startLines | ForEach-Object { $_ | ConvertFrom-Json })
    $opened = @($rows | Where-Object { $_.event -eq 'store_opened' })
    $errors = @($rows | Where-Object { $_.event -match '(error|failed)$' -or ($_.event -eq 'store_check' -and -not $_.success) })
    $result = [ordered]@{ data_directory=$DataDirectory; duration_seconds=$timer.Elapsed.TotalSeconds; opened=$opened; errors=$errors; events=@($rows | ForEach-Object { $_.event }); memory=$samplesTaken; live_snapshot=$live; owned_pids=@($owned.Keys) }
    $result | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $Output -Encoding utf8
    if ($opened.Count -ne 1 -or $errors.Count -ne 0) { throw 'smoke_log_validation_failed' }
} finally {
    Update-OwnedProcesses @(Get-CimInstance Win32_Process)
    # PID 与创建时间同时匹配，防止已退出的子进程 PID 被别的进程复用。
    foreach ($processId in @($owned.Keys | Sort-Object -Descending)) {
        $process = Get-CimInstance Win32_Process -Filter ('ProcessId = ' + $processId)
        if ($process -and $process.CreationDate -eq $owned[$processId]) { Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue }
    }
    if ($app) { $app.Dispose() }
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}
Write-Output ('smoke_results=' + $Output)
