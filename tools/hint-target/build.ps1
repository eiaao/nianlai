param([string]$OutputDirectory = (Join-Path $PSScriptRoot '..\..\release\hint-target'))
$ErrorActionPreference = 'Stop'
$compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
if (-not (Test-Path -LiteralPath $compiler)) { throw '未找到 .NET Framework 4 的 C# 编译器' }
$output = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $output -Force | Out-Null
$exe = Join-Path $output 'HintTarget.exe'
& $compiler /nologo /warnaserror /platform:x64 /target:winexe "/out:$exe" /reference:System.Windows.Forms.dll /reference:System.Drawing.dll /reference:System.Web.Extensions.dll (Join-Path $PSScriptRoot 'Program.cs') (Join-Path $PSScriptRoot 'EvidenceOutput.cs') (Join-Path $PSScriptRoot 'FileIoTests.cs')
if ($LASTEXITCODE -ne 0) { throw "验收目标编译失败：$LASTEXITCODE" }
# 使用启动进程并等待的方式获取 WinExe 自测退出码，不创建 GUI。
$check = [Diagnostics.ProcessStartInfo]::new($exe, '--self-test')
$check.UseShellExecute = $false
$check.CreateNoWindow = $true
$check.RedirectStandardOutput = $true
$check.RedirectStandardError = $true
$check.StandardOutputEncoding = [Text.Encoding]::UTF8
$check.StandardErrorEncoding = [Text.Encoding]::UTF8
$process = [Diagnostics.Process]::Start($check)
$stdout = $process.StandardOutput.ReadToEnd()
$stderr = $process.StandardError.ReadToEnd()
$process.WaitForExit()
if ($stdout) { Write-Output $stdout.Trim() }
if ($process.ExitCode -ne 0) { throw "验收目标自测失败：$stderr" }
Write-Output "已构建并完成无窗口自测：$exe"
Get-FileHash -LiteralPath $exe -Algorithm SHA256
