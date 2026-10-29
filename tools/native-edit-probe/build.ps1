param([Parameter(Mandatory)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not [IO.Path]::IsPathRooted($OutputDirectory)) { throw 'Absolute output directory required.' }
$buildRoot = [IO.Path]::GetFullPath($OutputDirectory)
$compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
$source = Join-Path $PSScriptRoot 'Program.cs'
$executable = Join-Path $buildRoot 'NativeEditProbe.exe'
function Hash([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
function SaveNew([string]$Path, [string]$Text) {
    $stream = [IO.FileStream]::new($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    try {
        $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false))
        try { $writer.Write($Text) } finally { $writer.Dispose() }
    } finally { $stream.Dispose() }
}
if (Test-Path -LiteralPath (Join-Path $buildRoot 'manifest.json')) { throw 'Build manifest already frozen; do not overwrite.' }
if (-not (Test-Path -LiteralPath $buildRoot)) { $null = New-Item -ItemType Directory -Path $buildRoot }
$attempt = 1
while (Test-Path -LiteralPath (Join-Path $buildRoot ('compile-' + $attempt + '.log'))) { $attempt++ }
$compileLog = Join-Path $buildRoot ('compile-' + $attempt + '.log')
$arguments = @('/nologo','/warnaserror','/codepage:65001','/optimize+','/platform:x64','/target:exe',
    '/reference:System.Windows.Forms.dll','/reference:System.Drawing.dll','/reference:System.Web.Extensions.dll',
    ('/out:' + $executable),$source)
$compileText = & $compiler @arguments 2>&1
$compileExit = $LASTEXITCODE
SaveNew $compileLog (($compileText | ForEach-Object { [string]$_ }) -join [Environment]::NewLine)
if ($compileExit -ne 0) { throw ('Compile failed: ' + $compileLog) }
$selfTestPath = Join-Path $buildRoot ('self-test-' + $attempt + '.json')
$selfTestError = Join-Path $buildRoot ('self-test-' + $attempt + '.stderr')
$start = [Diagnostics.ProcessStartInfo]::new($executable, '--self-test')
$start.UseShellExecute = $false; $start.CreateNoWindow = $true; $start.WindowStyle = 'Hidden'
$start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
$test = [Diagnostics.Process]::Start($start)
try {
    $stdoutTask = $test.StandardOutput.ReadToEndAsync()
    $stderrTask = $test.StandardError.ReadToEndAsync()
    if (-not $test.WaitForExit(10000)) { throw 'Pure self-test did not exit within 10 seconds.' }
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
    $selfTestExit = $test.ExitCode
} finally { $test.Dispose() }
SaveNew $selfTestPath $stdout
SaveNew $selfTestError $stderr
$selfTest = $stdout | ConvertFrom-Json
if ($selfTestExit -ne 0 -or -not $selfTest.ok -or $selfTest.native_api_calls -ne 0 -or
    $selfTest.controls_created -ne 0 -or $selfTest.child_processes_started -ne 0 -or $selfTest.live_modes_executed -ne 0) {
    throw 'Pure self-test failed or reached a live path.'
}
$sources = @('Program.cs','build.ps1','README.md' | ForEach-Object {
    $path = Join-Path $PSScriptRoot $_
    [ordered]@{name=$_;path=$path;sha256=(Hash $path)}
})
$manifest = [ordered]@{
    schema=1; status='compiled_and_pure_selftest_passed_live_not_run'; built_at=[DateTimeOffset]::Now.ToString('o');
    product_baseline='f3664d418039d06f49055bbe310fa702890ebdf0'; product_sources_modified=$false;
    compiler=$compiler; compiler_sha256=(Hash $compiler); compiler_arguments=$arguments; compile_exit_code=$compileExit;
    compile_log=$compileLog; compile_log_sha256=(Hash $compileLog); executable=$executable; executable_sha256=(Hash $executable);
    source_files=$sources; self_test=$selfTest; self_test_path=$selfTestPath; self_test_sha256=(Hash $selfTestPath);
    self_test_stderr_path=$selfTestError; self_test_stderr_sha256=(Hash $selfTestError); self_test_exit_code=$selfTestExit;
    live_run_executed=$false; clipboard_accessed=$false; controls_created=$false; child_fixture_processes_started=$false
}
$manifestPath = Join-Path $buildRoot 'manifest.json'
SaveNew $manifestPath ($manifest | ConvertTo-Json -Depth 10)
SaveNew (Join-Path $buildRoot 'manifest.sha256') (Hash $manifestPath)
[ordered]@{executable=$executable;manifest_sha256=(Hash $manifestPath);pure_checks_passed=$selfTest.pure_checks_passed;live_run_executed=$false} | ConvertTo-Json
