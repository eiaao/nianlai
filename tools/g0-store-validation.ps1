param([string]$Binary = (Join-Path $PSScriptRoot '../target/release/g0-store.exe'))
$ErrorActionPreference = 'Stop'
$Binary = (Resolve-Path -LiteralPath $Binary).Path
$outputDirectory = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../target/g0-store'))
[IO.Directory]::CreateDirectory($outputDirectory) | Out-Null
$root = Join-Path ([IO.Path]::GetTempPath()) ('nianlai-g0-validation-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($root) | Out-Null
$previousDirectory = $env:NIANLAI_G0_DATA_DIR
$reports = [ordered]@{ data_root=$root; measurements=[ordered]@{} }
function Measure-Store([string]$Name, [string[]]$Arguments) {
    $lines = & $Binary @Arguments
    if ($LASTEXITCODE -ne 0) { throw ('validation_failed: ' + $Name) }
    $value = $lines | ConvertFrom-Json
    $reports.measurements[$Name] = $value
    $value | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $outputDirectory ($Name + '.json')) -Encoding utf8
    $reports | ConvertTo-Json -Depth 16 | Set-Content -LiteralPath (Join-Path $outputDirectory 'validation-results.json') -Encoding utf8
    Write-Output ('completed=' + $Name)
}
try {
    foreach ($count in @(10000,100000)) {
        $env:NIANLAI_G0_DATA_DIR = Join-Path $root $count
        Measure-Store "fill-$count" @('fill','--count',"$count")
        Measure-Store "search-$count" @('search-bench','--iterations','100')
        Measure-Store "check-$count" @('check')
        Measure-Store "open-$count" @('open-bench')
    }
    Measure-Store 'snapshot-100000' @('snapshot')
    $snapshot = $reports.measurements['snapshot-100000'].report.file
    Measure-Store 'restore-100000' @('restore','--snapshot', $snapshot)
    Measure-Store 'restored-check-100000' @('check')
    $bad = Join-Path $root 'broken-snapshot.db'
    [IO.File]::Copy($snapshot, $bad)
    $stream = [IO.File]::Open($bad, [IO.FileMode]::Open, [IO.FileAccess]::Write)
    try { $stream.Write([Text.Encoding]::ASCII.GetBytes('broken snapshot header')) } finally { $stream.Dispose() }
    $before = (Get-FileHash -LiteralPath (Join-Path $env:NIANLAI_G0_DATA_DIR 'history.db') -Algorithm SHA256).Hash
    $badOutput = & $Binary restore --snapshot $bad 2>&1
    $badExit = $LASTEXITCODE
    $after = (Get-FileHash -LiteralPath (Join-Path $env:NIANLAI_G0_DATA_DIR 'history.db') -Algorithm SHA256).Hash
    $reports.measurements['corrupt-restore'] = [ordered]@{ exit_code=$badExit; rejected=($badExit -ne 0); original_unchanged=($before -eq $after) }
    if ($badExit -eq 0 -or $before -ne $after) { throw 'corrupt_snapshot_restore_changed_database' }
    Measure-Store 'after-rejection-check' @('check')
    $env:NIANLAI_G0_DATA_DIR = Join-Path $root 'blobs'
    Measure-Store 'blob-bench' @('blob-bench')
    Measure-Store 'blob-check' @('check')
} finally { $env:NIANLAI_G0_DATA_DIR = $previousDirectory }
Write-Output ('validation_results=' + (Join-Path $outputDirectory 'validation-results.json'))
