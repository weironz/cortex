#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$CandidateDriverPath,
    [Parameter(Mandatory=$true)][string]$CandidateExpectedSha256,
    [Parameter(Mandatory=$true)][string]$FixturePath,
    [Parameter(Mandatory=$true)][string]$FixtureExpectedSha256,
    [Parameter(Mandatory=$true)][string]$RunDirectory,
    [Parameter(Mandatory=$true)][string]$RunName
)
$options = @{ Candidate=$CandidateDriverPath; CandidateSha=$CandidateExpectedSha256; Fixture=$FixturePath; FixtureSha=$FixtureExpectedSha256; Root=$RunDirectory; Name=$RunName }
. (Join-Path $PSScriptRoot 'actions-lib.ps1')
$result = New-CAReport
$result.run_name=$options.Name; $result.harness_pid=$PID
$result.owner_run_id=[Environment]::GetEnvironmentVariable('CORTEX_CUA_ACTIONS_OWNER_RUN_ID')
$pins=[Collections.Generic.List[object]]::new()
$fixture=$null; $daemon=$null; $copyPath=$null; $root=$null
$clock=[Diagnostics.Stopwatch]::StartNew()
try {
    Require-CA ($env:OS -ceq 'Windows_NT' -and $result.owner_run_id -cmatch '^[0-9a-f]{32}$') 'CA_OWNED_ENTRY_REQUIRED'
    Assert-CAPath $options.Candidate; Assert-CAPath $options.Fixture
    Assert-SteadyPathChain $options.Root
    Require-CA ([IO.Directory]::Exists($options.Root) -and
        [IO.Path]::GetPathRoot($options.Root) -ieq [IO.Path]::GetPathRoot([Environment]::GetFolderPath('Windows'))) 'CA_PRIVATE_C_DIRECTORY'
    $root=[IO.Path]::GetFullPath($options.Root)
    Require-CA (-not [IO.File]::Exists((Join-Path $root 'summary.json'))) 'CA_OUTPUT_EXISTS'
    Initialize-CANative
    Require-CA ([CortexCandidateActions.Native]::InJob()) 'CA_JOB_REQUIRED'
    $driver=Open-ABPinnedFile $options.Candidate $options.CandidateSha; $pins.Add($driver)
    $source=Open-ABPinnedFile $options.Fixture $options.FixtureSha; $pins.Add($source)
    Require-CA (-not [string]::Equals($driver.Path,$source.Path,[StringComparison]::OrdinalIgnoreCase)) 'CA_BINARY_PATHS_EQUAL'
    $result.candidate_sha256=$driver.Sha256; $result.fixture_sha256=$source.Sha256
    $nonce=[guid]::NewGuid().ToString('N')
    $marker='CA-ACTIONS-'+$nonce; $initial='CA-INITIAL-'+$nonce; $baseline='CA-READY-'+$nonce
    $hash=[Security.Cryptography.SHA256]::Create()
    try { $result.marker_sha256=[BitConverter]::ToString($hash.ComputeHash([Text.Encoding]::UTF8.GetBytes($marker))).Replace('-','').ToLowerInvariant() }
    finally { $hash.Dispose() }
    $environment=New-ABEnvironment (Join-Path $root 'driver-home')
    $version=Invoke-ABCLI $driver @('--version') '' $environment 10000 $result
    Require-CA ($version.Text.Trim() -ceq 'cua-driver 0.38.0') 'CA_VERSION_REQUIRED'
    $result.version_verified=$true
    $copyPath=Join-Path $root ('fixture-'+$nonce+'.exe')
    $stream=[IO.File]::Open($copyPath,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
    try { $source.Stream.CopyTo($stream) } finally { $stream.Dispose() }
    $copy=Open-ABPinnedFile $copyPath $options.FixtureSha; $pins.Add($copy)
    $manifest=New-CAManifest $copy.Path
    Assert-CAManifest $manifest $copy.Path
    $manifestPath=Join-Path $root 'manifest.json'
    Write-SteadyJson $manifestPath $manifest
    Require-CA ([CuaReadonlyAB.Windows]::InteractiveDesktop()) 'CA_DESKTOP_NOT_INTERACTIVE'
    $fixture=Start-ABOwnedProcess $copy.Path @('--no-secure-field','--message',$initial,'--status',$baseline,'--exit-timeout-ms','150000') @{} 0 'fixture'
    $fixture.Process.StandardInput.Close()
    $startup=[Diagnostics.Stopwatch]::StartNew(); $window=0L
    while ($startup.ElapsedMilliseconds -lt 15000 -and $window -eq 0) {
        Assert-ABOwnedProcess $fixture
        $windows=[CuaReadonlyAB.Windows]::VisibleWindows($fixture.Process)
        Require-CA ($windows.Length -le 1) 'CA_WINDOW_COUNT'
        if ($windows.Length -eq 1) { $window=$windows[0] } else { Start-Sleep -Milliseconds 50 }
    }
    Require-CA ($window -ne 0) 'CA_WINDOW_TIMEOUT'
    Assert-ABFixtureWindow $fixture $window
    $socket='\\.\pipe\cortex-candidate-actions-'+$nonce
    $pidPath=Join-Path $root 'daemon.pid'
    $result.daemon_start_attempts++
    $daemon=Start-ABOwnedProcess $driver.Path @('serve','--permission-mode','bounded','--capability-manifest',$manifestPath,
        '--approve-capability-manifest','--socket',$socket,'--pid-file',$pidPath,'--no-overlay') $environment 0 'daemon'
    $daemon.Process.StandardInput.Close()
    $startup.Restart(); $ready=$false
    while ($startup.ElapsedMilliseconds -lt 15000 -and -not $ready) {
        Assert-ABOwnedProcess $daemon
        $ready=[CuaReadonlyAB.Windows]::PipeReady($socket)
        if (-not $ready) { Start-Sleep -Milliseconds 50 }
    }
    Require-CA $ready 'CA_DAEMON_TIMEOUT'
    $status=Invoke-ABCLI $driver @('status','--socket',$socket,'--pid-file',$pidPath) '' $environment 10000 $result
    Assert-ABStatus $status.Text; $result.bounded_status_verified=$true
    $result.fixture_pid=$fixture.Pid; $result.window_id=$window
    $context=[pscustomobject]@{
        Result=$result; Fixture=$fixture; FixturePid=$fixture.Pid; WindowId=$window; Daemon=$daemon
        Driver=$driver; Environment=$environment; Socket=$socket; Clock=$clock; CallMs=10000
        Session=('actions-'+$nonce); Marker=$marker; Initial=$initial; Baseline=$baseline
        InputToken=$null; ButtonToken=$null; Seen=[Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        IO=@{
            Now={param($c) return $c.Clock.ElapsedMilliseconds}
            Check={param($c,$cleanup) Assert-ABOwnedProcess $c.Daemon; if (-not $cleanup) { Assert-ABFixtureWindow $c.Fixture $c.WindowId }}
            Read={
                param($c)
                Assert-ABFixtureWindow $c.Fixture $c.WindowId
                $left=90000-$c.Clock.ElapsedMilliseconds
                Require-CA ($left -gt 0) 'CA_DEADLINE'
                return [CortexCandidateActions.Native]::ReadBounded($c.WindowId,$c.FixturePid,[int][Math]::Min(5000,$left))
            }
            Call={
                param($c,$tool,$arguments,$deadline)
                $reply=Invoke-ABCLI $c.Driver @('call','--socket',$c.Socket,$tool) ($arguments|ConvertTo-Json -Compress -Depth 8) $c.Environment $deadline $c.Result
                try { $payload=ConvertFrom-Json -InputObject $reply.Text -ErrorAction Stop }
                catch { throw [InvalidOperationException]::new('CA_REPLY_JSON') }
                return [pscustomobject]@{Text='';Payload=$payload}
            }
        }
    }
    Invoke-CASequence $context
}
catch { $result.failure_code=Get-CAError $_ }
finally {
    $result.daemon_cleanup_ok=Stop-ABOwnedProcess $daemon $false
    if ($null -ne $daemon) { $result.daemon_exit_code=$daemon.ExitCode }
    $result.fixture_cleanup_ok=Stop-ABOwnedProcess $fixture $true
    if ($null -ne $fixture) { $result.fixture_exit_code=$fixture.ExitCode }
    foreach ($pin in $pins) { $pin.Stream.Dispose() }
    if ($null -ne $copyPath -and $result.fixture_cleanup_ok) {
        try {
            Assert-SteadyPathChain $root
            Require-CA (([IO.File]::GetAttributes($copyPath)-band[IO.FileAttributes]::ReparsePoint)-eq 0) 'CA_COPY_LINK'
            [IO.File]::Delete($copyPath); $result.fixture_copy_removed=-not [IO.File]::Exists($copyPath)
        }
        catch { $result.cleanup_failure_code='CA_FIXTURE_COPY_CLEANUP' }
    }
    try { Assert-CACompleted $result; $result.status='PASS' }
    catch { if ($null -eq $result.failure_code) { $result.failure_code=Get-CAError $_ } }
    if ($null -ne $root) { Write-SteadyJson (Join-Path $root 'summary.json') $result }
}
Write-Output ('CA_ACTIONS_'+$result.status+' scope=unsigned_candidate_fixture_actions_investigation')
if ($result.status -cne 'PASS') { exit 1 }
