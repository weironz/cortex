#requires -Version 5.1
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -Path (Join-Path $PSScriptRoot 'owned-job.cs')
$job = [CortexReadonlyOwned.OwnedJob]::new()
$child = $null
$root = Join-Path ([IO.Path]::GetTempPath()) ('cortex-job-selftest-' + [guid]::NewGuid().ToString('N'))
$null = [IO.Directory]::CreateDirectory($root)
$log = Join-Path $root 'child.log'
$envMap = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) { $envMap[[string]$entry.Key] = [string]$entry.Value }
$shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
# 只创建本测试的两个无 GUI 进程；孙进程必须继承同一个原 Job。
$code = '$s=[Diagnostics.ProcessStartInfo]::new(); $s.FileName=[Diagnostics.Process]::GetCurrentProcess().MainModule.FileName; $s.Arguments="-NoProfile -NonInteractive -Command Start-Sleep -Seconds 60"; $s.UseShellExecute=$false; $s.CreateNoWindow=$true; $p=[Diagnostics.Process]::Start($s); Start-Sleep -Seconds 60'
try {
    $child = $job.Start($shell, [string[]]@('-NoProfile','-NonInteractive','-Command',$code), $root, $log, $true, $envMap)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($job.ActiveCount -lt 2 -and $clock.ElapsedMilliseconds -lt 10000) { [Threading.Thread]::Sleep(25) }
    if ($job.ActiveCount -lt 2) { throw 'OWNED_JOB_GRANDCHILD_NOT_PRESENT' }
    if (-not $job.StopAndWait(10000) -or $job.ActiveCount -ne 0 -or -not $child.Wait(5000)) {
        throw 'OWNED_JOB_DRAIN_NOT_PROVEN'
    }
    Write-Output 'OWNED_JOB_SELFTEST_PASS root_and_grandchild=true original_job_active_zero=true gui_driver_started=0'
}
finally {
    try { $null = $job.StopAndWait(10000) }
    finally { $job.Dispose(); if ($null -ne $child) { $child.Dispose() } }
}
