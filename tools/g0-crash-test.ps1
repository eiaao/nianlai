param(
    [int]$Rounds = 30,
    [string]$Binary = (Join-Path $PSScriptRoot '../target/release/g0-store.exe'),
    [string]$Output = (Join-Path $PSScriptRoot '../target/g0-store/crash-results.json')
)
$ErrorActionPreference = 'Stop'
$Binary = (Resolve-Path -LiteralPath $Binary).Path
$Output = [IO.Path]::GetFullPath($Output)
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Output)) | Out-Null
$dataDirectory = Join-Path ([IO.Path]::GetTempPath()) ('nianlai-g0-crash-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($dataDirectory) | Out-Null
$previousDirectory = $env:NIANLAI_G0_DATA_DIR
$env:NIANLAI_G0_DATA_DIR = $dataDirectory
$random = [Random]::new(20261024)
$results = [Collections.Generic.List[object]]::new()
try {
    for ($round = 1; $round -le $Rounds; $round++) {
        $stdout = Join-Path $dataDirectory ('writer-' + $round + '.out')
        $stderr = Join-Path $dataDirectory ('writer-' + $round + '.err')
        $delay = $random.Next(200, 3001)
        $timer = [Diagnostics.Stopwatch]::StartNew()
        $writer = Start-Process -FilePath $Binary -ArgumentList @('writer', '--seed', $round) -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        try {
            # 从首条已刷新的提交回执开始注入，避免仅杀在进程加载阶段而无法核对提交编号。
            $ready = [Diagnostics.Stopwatch]::StartNew()
            while (-not (Test-Path -LiteralPath $stdout) -or (Get-Item -LiteralPath $stdout).Length -eq 0) {
                if ($writer.HasExited) { throw 'writer_exited_before_ready' }
                if ($ready.Elapsed.TotalSeconds -gt 30) { throw 'writer_ready_timeout' }
                Start-Sleep -Milliseconds 10
            }
            $startupMs = $ready.Elapsed.TotalMilliseconds
            Start-Sleep -Milliseconds $delay
            if ($writer.HasExited) { throw 'writer_exited_before_injection' }
            # 仅结束这一轮由本脚本启动的进程对象对应的 PID。
            Stop-Process -Id $writer.Id -Force
            $writer.WaitForExit()
        } finally {
            if (-not $writer.HasExited) { Stop-Process -Id $writer.Id -Force; $writer.WaitForExit() }
            $writer.Dispose()
        }
        $last = Get-Content -LiteralPath $stdout -Tail 1
        $lastId = 0L
        if (-not [long]::TryParse($last, [ref]$lastId)) { throw 'no_complete_commit_acknowledgement' }
        $checkOutput = & $Binary check --last-id $lastId
        $checkExit = $LASTEXITCODE
        $checked = if ($checkExit -eq 0) { $checkOutput | ConvertFrom-Json } else { $null }
        $ok = $checkExit -eq 0 -and $checked.ok -and $checked.last_committed_present
        $row = [ordered]@{ round=$round; startup_wait_ms=$startupMs; delay_ms=$delay; last_committed_id=$lastId; check_exit=$checkExit; ok=$ok; elapsed_ms=$timer.Elapsed.TotalMilliseconds; counts=$checked.counts }
        $results.Add($row)
        [ordered]@{ rounds=$Rounds; completed=$results.Count; data_directory=$dataDirectory; results=$results } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $Output -Encoding utf8
        Write-Output ('round={0} check={1} last_committed_id={2}' -f $round, $ok, $lastId)
    }
} finally {
    $env:NIANLAI_G0_DATA_DIR = $previousDirectory
}
if ($results.Count -ne $Rounds -or @($results | Where-Object { -not $_.ok }).Count -gt 0) { throw 'crash_test_failed' }
Write-Output ('30-round result file: ' + $Output)
