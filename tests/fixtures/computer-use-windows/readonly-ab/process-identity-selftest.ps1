param([switch]$ChildProbe)
$ErrorActionPreference = 'Stop'
if (-not $ChildProbe) {
    Add-Type -Path (Join-Path $PSScriptRoot 'owned-job.cs')
    $envMap = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($key in @('SystemRoot','WINDIR','PATH','USERPROFILE','LOCALAPPDATA','TEMP','TMP','PSModulePath')) {
        $value=[Environment]::GetEnvironmentVariable($key)
        if ($null -ne $value) { $envMap[$key]=$value }
    }
    $root=Join-Path ([IO.Path]::GetTempPath()) ('cortex-identity-selftest-' + [guid]::NewGuid().ToString('N'))
    $null=[IO.Directory]::CreateDirectory($root)
    $log=Join-Path $root 'identity.log'
    $job=[CortexReadonlyOwned.OwnedJob]::new(); $child=$null; $ok=$false
    try {
        $shell=Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
        $child=$job.Start($shell,[string[]]@('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',$PSCommandPath,'-ChildProbe'),$root,$log,$true,$envMap)
        $ok=$child.Wait(60000) -and $child.ExitCode -eq 0
    }
    finally {
        try { $drained=$job.StopAndWait(10000) }
        finally { $job.Dispose(); if($null -ne $child){$child.Dispose()} }
    }
    Get-Content -LiteralPath $log -Encoding UTF8
    if(-not $ok -or -not $drained){throw 'IDENTITY_SELFTEST_OR_JOB_FAILED'}
    Write-Output 'IDENTITY_SELFTEST_JOB_PASS original_job_active_zero=true'
    return
}
. (Join-Path $PSScriptRoot 'cua-readonly-ab.ps1') -SourceOnly
Initialize-ABNative
$shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
for ($i=0; $i -lt 30; $i++) {
    $owner=$null
    try {
        $owner=Start-ABOwnedProcess $shell @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 60') @{} 0 'identity_probe'
        Assert-ABOwnedProcess $owner
        $path=$owner.Path; $owner.Path='C:\not-the-created-executable.exe'
        $rejected=$false
        try { Assert-ABOwnedProcess $owner } catch { $rejected=$_.Exception.Message -ceq 'AB_PROCESS_IDENTITY_IMAGE' }
        finally { $owner.Path=$path }
        if(-not $rejected){throw 'IDENTITY_IMAGE_MISMATCH_ACCEPTED'}
        $ticks=$owner.StartTicks; $owner.StartTicks=$ticks+1
        $rejected=$false
        try { Assert-ABOwnedProcess $owner } catch { $rejected=$_.Exception.Message -ceq 'AB_PROCESS_IDENTITY_START_TIME' }
        finally { $owner.StartTicks=$ticks }
        if(-not $rejected){throw 'IDENTITY_CREATION_MISMATCH_ACCEPTED'}
        $number=$owner.Pid; $owner.Pid=$number+1
        $rejected=$false
        try { Assert-ABOwnedProcess $owner } catch { $rejected=$_.Exception.Message -ceq 'AB_PROCESS_IDENTITY_PID' }
        finally { $owner.Pid=$number }
        if(-not $rejected){throw 'IDENTITY_PID_MISMATCH_ACCEPTED'}
    }
    finally {
        if($null -ne $owner -and -not (Stop-ABOwnedProcess $owner $false)){throw 'IDENTITY_CHILD_CLEANUP_FAILED'}
    }
}
Write-Output 'IDENTITY_PROBE_PASS startup_cycles=30 image_pid_creation_mismatch_rejected=true gui_driver_started=0'
