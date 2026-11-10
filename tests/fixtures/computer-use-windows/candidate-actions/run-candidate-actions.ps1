#requires -Version 5.1
[CmdletBinding(DefaultParameterSetName='Run')]
param(
    [Parameter(Mandatory=$true,ParameterSetName='Run')][string]$CandidateDriverPath,
    [Parameter(Mandatory=$true,ParameterSetName='Run')][string]$CandidateExpectedSha256,
    [Parameter(Mandatory=$true,ParameterSetName='Run')][string]$FixturePath,
    [Parameter(Mandatory=$true,ParameterSetName='Run')][string]$FixtureExpectedSha256,
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9_-]{0,47}$')][string]$RunName=([guid]::NewGuid().ToString('N')),
    [Parameter(Mandatory=$true,ParameterSetName='Validate')][switch]$ValidateOnly
)
$mode=$PSCmdlet.ParameterSetName
$options=@{Candidate=$CandidateDriverPath;CandidateSha=$CandidateExpectedSha256;Fixture=$FixturePath;FixtureSha=$FixtureExpectedSha256;Name=$RunName}
. (Join-Path $PSScriptRoot 'actions-lib.ps1')
$jobSource=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\readonly-ab\owned-job.cs'))
if ($mode -ceq 'Validate') {
    Add-Type -Path $jobSource
    Initialize-CANative
    Write-Output 'CA_ACTIONS_SOURCE_PASS native=compile_only jobs_started=0 gui_driver_started=0'
    return
}
Assert-CAPath $options.Candidate; Assert-CAPath $options.Fixture
$pins=[Collections.Generic.List[object]]::new()
$job=$null; $child=$null; $root=$null; $lease=$null; $drained=$false; $code=1
$ownerId=[guid]::NewGuid().ToString('N')
$report=[ordered]@{schema_version=1;scope='owned_unsigned_candidate_fixture_actions';status='FAIL';run_id=$ownerId;child_pid=$null
    investigation_only=$true;product_signature_gate_passed=$false;product_http_fixture_gate_passed=$false
    original_job_drained=$false;child_exit_code=$null;source_changed=$false;sources=[ordered]@{};failure_code=$null}
try {
    Require-CA ($env:OS -ceq 'Windows_NT') 'CA_WINDOWS_REQUIRED'
    $parent=Join-Path ([IO.Path]::GetTempPath()) 'CortexCandidateActions'
    Require-CA ([IO.Path]::GetPathRoot($parent) -ieq [IO.Path]::GetPathRoot([Environment]::GetFolderPath('Windows'))) 'CA_C_TEMP_REQUIRED'
    $root=New-SteadyRunDirectory $parent $options.Name
    $lease=[IO.File]::Open((Join-Path $root 'run.lock'),[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
    $driver=Open-ABPinnedFile $options.Candidate $options.CandidateSha; $pins.Add($driver)
    $fixture=Open-ABPinnedFile $options.Fixture $options.FixtureSha; $pins.Add($fixture)
    $worker=Join-Path $script:CAHome 'probe.ps1'
    $sources=@($PSCommandPath,$worker,(Join-Path $script:CAHome 'actions-lib.ps1'),(Join-Path $script:CAHome 'actions-native.cs'),
        (Join-Path $script:CAHome '..\readonly-ab\cua-readonly-ab.ps1'),(Join-Path $script:CAHome '..\readonly-ab\cua-readonly-steady.ps1'),$jobSource)
    foreach ($source in $sources) {
        $sha=Get-CAFileSha256 $source
        $pin=Open-ABPinnedFile $source $sha; $pins.Add($pin); $report.sources[$pin.Path]=$pin.Sha256
    }
    Add-Type -Path $jobSource
    $environment=[Collections.Generic.Dictionary[string,string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) { $environment[[string]$entry.Key]=[string]$entry.Value }
    $environment['CORTEX_CUA_ACTIONS_OWNER_RUN_ID']=$ownerId
    $shell=Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $arguments=@('-NoProfile','-NonInteractive','-File',$worker,'-CandidateDriverPath',$driver.Path,'-CandidateExpectedSha256',$driver.Sha256,
        '-FixturePath',$fixture.Path,'-FixtureExpectedSha256',$fixture.Sha256,'-RunDirectory',$root,'-RunName',$options.Name)
    $job=[CortexReadonlyOwned.OwnedJob]::new()
    $child=$job.Start($shell,[string[]]$arguments,$script:CAHome,(Join-Path $root 'child.log'),$true,$environment)
    $report.child_pid=$child.Id
    Write-Output ('CA_ACTIONS_RUNNING child_pid='+$child.Id+' output_directory='+$root)
    $clock=[Diagnostics.Stopwatch]::StartNew()
    while (-not $child.Wait(500)) { Require-CA ($clock.ElapsedMilliseconds -lt 180000) 'CA_OWNER_HARD_DEADLINE' }
    $code=$child.ExitCode; $report.child_exit_code=$code
}
catch { $report.failure_code=Get-CAError $_ }
finally {
    if ($null -ne $job) {
        try { $drained=$job.StopAndWait(10000) }
        catch { $report.failure_code='CA_JOB_DRAIN_UNKNOWN' }
        finally { $job.Dispose() }
    }
    if ($null -ne $child) { $child.Dispose() }
    $report.original_job_drained=$drained
    foreach ($entry in $report.sources.GetEnumerator()) {
        if ((Get-CAFileSha256 $entry.Key) -cne $entry.Value) { $report.source_changed=$true }
    }
    foreach ($pin in $pins) { $pin.Stream.Dispose() }
    if ($null -ne $root) {
        try {
            $summary=[IO.File]::ReadAllText((Join-Path $root 'summary.json'),[Text.Encoding]::UTF8) | ConvertFrom-Json
            Require-CA ($summary.owner_run_id -ceq $ownerId -and $summary.harness_pid -eq $report.child_pid -and
                $summary.scope -ceq 'unsigned_candidate_fixture_actions_investigation') 'CA_OWNER_EVIDENCE_BINDING'
            Assert-CACompleted $summary
            Require-CA ($null -eq $report.failure_code -and $code -eq 0 -and $drained -and -not $report.source_changed -and $summary.status -ceq 'PASS') 'CA_OWNER_INCOMPLETE'
            $report.status='PASS'
        }
        catch { if ($null -eq $report.failure_code) { $report.failure_code=Get-CAError $_ } }
        try { Write-SteadyJson (Join-Path $root 'owner.json') $report }
        finally { if ($null -ne $lease) { $lease.Dispose() } }
    }
}
Write-Output ('CA_ACTIONS_OWNER_'+$report.status+' original_job_drained='+$drained)
if ($report.status -cne 'PASS') { exit 1 }
