#requires -Version 5.1
[CmdletBinding()]
param([switch]$JsonFileOnly, [switch]$DiagnosticsOnly)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$path = Join-Path $PSScriptRoot 'cua-readonly-ab.ps1'
$steadyPath = Join-Path $PSScriptRoot 'cua-readonly-steady.ps1'
foreach ($file in @($path, $steadyPath, (Join-Path $PSScriptRoot 'steady-selftest.ps1'), $MyInvocation.MyCommand.Path)) {
    $bytes = [IO.File]::ReadAllBytes($file)
    if ($bytes.Length -lt 3 -or $bytes[0] -ne 239 -or $bytes[1] -ne 187 -or $bytes[2] -ne 191) {
        throw 'SOURCE_UTF8_BOM_REQUIRED'
    }
    $tokens = $null
    $parseErrors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile($file, [ref]$tokens, [ref]$parseErrors)
    if ($parseErrors.Count -ne 0) { throw 'SOURCE_SYNTAX_FAILED' }
    $forbidden = @($ast.FindAll({
        param($node)
        $node -is [Management.Automation.Language.CommandAst] -and
            $node.GetCommandName() -in @('Start-Process', 'Stop-Process', 'Get-Process', 'Invoke-Expression')
    }, $true))
    if ($forbidden.Count -ne 0) { throw 'SOURCE_FORBIDDEN_PROCESS_COMMAND' }
}

if ($JsonFileOnly -or $DiagnosticsOnly) {
    . $steadyPath -SourceOnly
    . (Join-Path $PSScriptRoot 'steady-selftest.ps1')
    if ($DiagnosticsOnly) { Invoke-SteadyDiagnosticsSelfTests }
    Invoke-SteadyJsonFileSelfTest
    if ($DiagnosticsOnly) {
        Initialize-ABNative
        Invoke-ABDesktopClassificationSelfTests
        Invoke-SteadyOutputSelfTests
    }
    $nativeMode = 'not_loaded'
    if ($DiagnosticsOnly) { $nativeMode = 'compile_only' }
    Write-Output ('SOURCE_VALIDATION_PASS syntax=BOM+PS51-compatible json_file=private_temp native=' + $nativeMode + ' gui_driver_started=0')
    return
}

# 两条入口都在任何文件/进程/窗口操作之前返回。
. $path -SelfTest
. $steadyPath -SelfTest
Invoke-SteadyJsonFileSelfTest
Invoke-SteadyDiagnosticsSelfTests
# 只编译声明；桌面自测仅传入合成证据，不调用 user32/kernel32/WTS。
Initialize-ABNative
Invoke-ABDesktopClassificationSelfTests
Invoke-SteadyOutputSelfTests
foreach ($limit in @(4, 0)) {
    $bytes = [Text.Encoding]::UTF8.GetBytes('synthetic-private-pipe-data')
    $memory = [IO.MemoryStream]::new($bytes)
    $reader = [IO.StreamReader]::new($memory, [Text.Encoding]::UTF8)
    try {
        $pump = [CuaReadonlyAB.Pump]::new($reader, $limit)
        Require-AB ($pump.Completion.Wait(2000)) 'SOURCE_MEMORY_PUMP_TIMEOUT'
        Require-AB ($pump.Count -eq $bytes.Length) 'SOURCE_MEMORY_PUMP_COUNT'
        if ($limit -eq 4) {
            Require-AB ($pump.Text.Length -eq 4 -and $pump.Overflow) 'SOURCE_MEMORY_PUMP_BOUND'
        }
        else {
            Require-AB ($pump.Text -ceq '' -and -not $pump.Overflow) 'SOURCE_MEMORY_PUMP_DISCARD'
        }
    }
    finally { $reader.Dispose(); $memory.Dispose() }
}
Write-Output 'SOURCE_VALIDATION_PASS syntax=BOM+PS51-compatible native=compile_only streams=memory_only json_file=private_temp gui_driver_started=0'
