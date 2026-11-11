param([Parameter(Mandatory = $true)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
if (-not (Test-Path -LiteralPath $compiler)) { throw '未找到 .NET Framework 4 C# 编译器' }
$output = [IO.Path]::GetFullPath($OutputDirectory)
$null = [IO.Directory]::CreateDirectory($output)
$exe = Join-Path $output 'NativePasteProbe.exe'
if (Test-Path -LiteralPath $exe) { throw '目标 exe 已存在，选择新的独立构建目录，禁止覆盖冻结产物' }
& $compiler /nologo /warnaserror /codepage:65001 /platform:x64 /target:winexe "/out:$exe" `
    /reference:System.Windows.Forms.dll /reference:System.Drawing.dll /reference:System.Web.Extensions.dll `
    (Join-Path $PSScriptRoot 'Isolation.cs') (Join-Path $PSScriptRoot 'Program.cs')
if ($LASTEXITCODE -ne 0) { throw "NativePasteProbe 编译失败：$LASTEXITCODE" }
# 本脚本绝不启动 helper；执行由外部驱动在已核验的私有 scope 或本轮明确许可的诊断 scope 中负责。
Get-FileHash -LiteralPath $exe -Algorithm SHA256
