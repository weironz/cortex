#requires -Version 5.1
[CmdletBinding()]
param([string]$ArgumentsFile, [ValidateSet('steady','build')][string]$Mode = 'steady',
    [ValidateRange(60,15300)][int]$HardTimeoutSeconds = 15300, [switch]$ValidateOnly)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
function Get-OwnedSha256 {
    param([string]$Path)
    $stream = [IO.File]::OpenRead($Path)
    $hash = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hash.ComputeHash($stream)).Replace('-','').ToLowerInvariant() }
    finally { $stream.Dispose(); $hash.Dispose() }
}
$native = Join-Path $PSScriptRoot 'owned-job.cs'
$childScript = Join-Path $PSScriptRoot 'cua-readonly-steady.ps1'
# 父进程仅启动固定的测试脚本；参数是 JSON 字符串数组，不解释命令文本。
Add-Type -Path $native
if ($ValidateOnly) {
    Write-Output 'OWNED_STEADY_SOURCE_PASS native=compile_only gui_driver_started=0'
    exit 0
}
if ([string]::IsNullOrWhiteSpace($ArgumentsFile)) { throw 'OWNED_STEADY_ARGUMENTS_REQUIRED' }
$values = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText((Resolve-Path -LiteralPath $ArgumentsFile)))
if ($values -isnot [array]) { throw 'OWNED_STEADY_ARGUMENTS_ARRAY_REQUIRED' }
if ($values.Count -eq 0 -or $values.Count -gt 64) { throw 'OWNED_STEADY_ARGUMENTS_COUNT' }
foreach ($value in $values) {
    if ($value -isnot [string] -or $value.Length -gt 4096 -or $value.IndexOf([char]0) -ge 0) {
        throw 'OWNED_STEADY_ARGUMENTS_TYPE'
    }
}
$runRoot = Join-Path ([IO.Path]::GetTempPath()) ('cortex-owned-steady-' + [guid]::NewGuid().ToString('N'))
$null = [IO.Directory]::CreateDirectory($runRoot)
$log = Join-Path $runRoot 'child.log'
$evidence = Join-Path $runRoot 'owner.json'
$environment = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) {
    $environment[[string]$entry.Key] = [string]$entry.Value
}
$ownerRunId = [guid]::NewGuid().ToString('N')
$environment['CORTEX_CUA_STEADY_RUN_ID'] = $ownerRunId
$shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$arguments = @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',$childScript) + [string[]]$values
$workingDirectory = $PSScriptRoot
if ($Mode -ceq 'build') {
    $shell = 'C:\Program Files\Git\bin\bash.exe'
    $workingDirectory = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..\..'))
    $arguments = @('scripts/run-python.sh','scripts/build-cua-candidate.py') + [string[]]$values
}
$sourcePaths = @($native, $PSCommandPath)
if ($Mode -ceq 'build') {
    $sourcePaths += @(Join-Path $workingDirectory 'scripts\build-cua-candidate.py')
    $sourcePaths += @(Join-Path $workingDirectory 'scripts\run-python.sh')
}
else {
    $sourcePaths += @($childScript, (Join-Path $PSScriptRoot 'cua-readonly-ab.ps1'))
}
$sourceBefore = @{}
foreach ($sourcePath in $sourcePaths) { $sourceBefore[$sourcePath] = Get-OwnedSha256 $sourcePath }
$job = [CortexReadonlyOwned.OwnedJob]::new()
$child = $null
$code = 1
$drained = $false
try {
    $child = $job.Start($shell, [string[]]$arguments, $workingDirectory, $log, $true, $environment)
    Write-Output ('OWNED_STEADY_RUNNING pid=' + $child.Id + ' log=' + $log)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while (-not $child.Wait(1000)) {
        if ($clock.Elapsed.TotalSeconds -ge $HardTimeoutSeconds) { throw 'OWNED_STEADY_HARD_DEADLINE' }
    }
    $code = $child.ExitCode
}
finally {
    # 先确认原 Job 的整树排空，再释放句柄；异常也不按名称/任意 PID 清理。
    try { $drained = $job.StopAndWait(10000) }
    finally {
        $job.Dispose()
        if ($null -ne $child) { $child.Dispose() }
        # Git Bash 可携带不同 PSModulePath；收尾证据不能依赖模块自动加载。
        $sourceChanged = $false
        foreach ($sourcePath in $sourcePaths) {
            if ((Get-OwnedSha256 $sourcePath) -cne $sourceBefore[$sourcePath]) { $sourceChanged = $true }
        }
        if ($sourceChanged) { $code = 1 }
        $record = @{ schema=2; scope=('owned_cua_' + $Mode + '_wrapper'); child_exit_code=$code;
            run_id=$ownerRunId; child_pid=$(if ($null -ne $child) { $child.Id } else { $null });
            original_job_drained=$drained; log=$log; helper_sha256=$sourceBefore[$native];
            sources=$sourceBefore; source_changed=$sourceChanged }
        [IO.File]::WriteAllText($evidence, ($record | ConvertTo-Json) + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))
    }
}
if (-not $drained) { throw 'OWNED_STEADY_JOB_DRAIN_UNKNOWN' }
Write-Output ('OWNED_STEADY_EXIT code=' + $code + ' original_job_drained=true evidence=' + $evidence)
exit $code
