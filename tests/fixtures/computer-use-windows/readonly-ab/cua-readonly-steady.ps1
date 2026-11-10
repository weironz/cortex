#requires -Version 5.1
[CmdletBinding(DefaultParameterSetName = 'Run')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Run')][string]$CandidateDriverPath,
    [Parameter(Mandatory = $true, ParameterSetName = 'Run')][string]$CandidateExpectedSha256,
    [Parameter(Mandatory = $true, ParameterSetName = 'Run')][string]$FixturePath,
    [Parameter(Mandatory = $true, ParameterSetName = 'Run')][string]$FixtureExpectedSha256,
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$')][string]$RunName = ([guid]::NewGuid().ToString('N')),
    [string]$OutputRoot = '',
    [ValidateRange(60, 14400)][int]$DurationSeconds = 14400,
    [ValidateRange(1, 200)][int]$WarmupObservations = 25,
    [ValidateRange(60, 900)][int]$SessionSeconds = 600,
    [ValidateRange(1, 500)][int]$SessionObservationLimit = 200,
    # 默认最快 RPC 下含预热约需 73 个会话；保留有界余量，不能依赖 RPC 足够慢。
    [ValidateRange(1, 256)][int]$MaxSessions = 128,
    [ValidateRange(0, 255)][int]$MinimumRotations = 0,
    [ValidateRange(100, 10000)][int]$ObservationIntervalMs = 1000,
    [ValidateRange(1000, 30000)][int]$RpcTimeoutMs = 20000,
    [ValidateRange(5, 60)][int]$SampleIntervalSeconds = 30,
    [Parameter(Mandatory = $true, ParameterSetName = 'SelfTest')][switch]$SelfTest,
    [Parameter(Mandatory = $true, ParameterSetName = 'SourceOnly')][switch]$SourceOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# dot-source 的参数与脚本变量会进入当前作用域，必须先保存本入口的参数。
$steadyMode = $PSCmdlet.ParameterSetName
$steadyOptions = @{
    CandidatePath = $CandidateDriverPath; CandidateSha = $CandidateExpectedSha256
    FixturePath = $FixturePath; FixtureSha = $FixtureExpectedSha256; RunName = $RunName; OutputRoot = $OutputRoot
    DurationSeconds = $DurationSeconds; Warmup = $WarmupObservations
    SessionSeconds = $SessionSeconds; SessionObservationLimit = $SessionObservationLimit
    MaxSessions = $MaxSessions; MinimumRotations = $MinimumRotations; IntervalMs = $ObservationIntervalMs; CallMs = $RpcTimeoutMs
    SampleSeconds = $SampleIntervalSeconds
}
. (Join-Path $PSScriptRoot 'cua-readonly-ab.ps1') -SourceOnly
$script:SteadyScope = 'driver_readonly_fixture_candidate_steady'

function New-SteadyPlan {
    param($Options)
    # 清理预留一个完整 RPC；不能在会话到期之后才想起 end_session。
    Require-AB ($Options.SessionSeconds * 1000 -gt (3 * $Options.CallMs + $Options.IntervalMs)) 'AB_STEADY_SESSION_HEADROOM'
    $minimum = Get-ABField $Options 'MinimumRotations'
    if ($null -eq $minimum) { $minimum = 0 }
    Require-AB ($minimum -lt $Options.MaxSessions) 'AB_STEADY_ROTATION_BUDGET'
    return [ordered]@{
        duration_ms = [long]$Options.DurationSeconds * 1000; warmup = $Options.Warmup
        session_ms = $Options.SessionSeconds * 1000; session_observations = $Options.SessionObservationLimit
        max_sessions = $Options.MaxSessions; minimum_rotations = [int]$minimum; interval_ms = $Options.IntervalMs; call_ms = $Options.CallMs
        sample_ms = $Options.SampleSeconds * 1000; setup_ms = 360000; cleanup_ms = 120000
        post_end_samples = 3; post_end_interval_ms = 1000
        manifest_seconds = $Options.DurationSeconds + 540
    }
}

function New-SteadyManifest {
    param([string]$FixtureExecutable, $Plan)
    $manifest = New-ABManifest $FixtureExecutable
    $manifest.expires_after = ([string]$Plan.manifest_seconds + 's')
    return $manifest
}

function Assert-SteadyManifest {
    param($Manifest, [string]$FixtureExecutable, $Plan)
    Require-AB ((Get-ABField $Manifest 'expires_after') -ceq ([string]$Plan.manifest_seconds + 's')) 'AB_STEADY_MANIFEST_LIFETIME'
    # 仅放宽独立候选入口的总有效期，其余资源、idle、工具契约仍由原断言检查。
    $copy = [ordered]@{}
    foreach ($entry in $Manifest.GetEnumerator()) { $copy[$entry.Key] = $entry.Value }
    $copy.expires_after = '35m'
    Assert-ABManifest $copy $FixtureExecutable
}

function New-SteadyResult {
    param($Plan)
    return [ordered]@{
        schema_version = 2; scope = $script:SteadyScope; status = 'RUNNING'; phase = 'setup'
        run_name = $null; harness_pid = $null; owner_run_id = $null; output_directory = $null
        investigation_only = $true; product_signature_gate_passed = $false
        memory_steady_state_asserted = $false; failure_code = $null; cleanup_failure_code = $null
        plan = $Plan; warmup_completed = 0; observations_completed = 0
        sessions_attempted = 0; sessions_started = 0; sessions_ended = 0; rotations = 0
        session_end_attempts = 0; max_seen_snapshots = 0; post_end_samples = 0
        cli_attempts = 0; cli_exit_codes = 0; cli_zero_exits = 0
        cli_stdout_chars = 0L; cli_stderr_chars = 0L
        observed_elements_total = 0L; observed_tree_chars_total = 0L
        warmup_elapsed_ms_total = 0.0; observation_elapsed_ms_total = 0.0
        steady_elapsed_ms = 0L; samples_written = 0; elapsed_ms = 0L
        daemon_pid = $null; daemon_start_utc_ticks = $null; daemon_exit_code = $null
        fixture_pid = $null; fixture_start_utc_ticks = $null; fixture_exit_code = $null
        candidate_sha256 = $null; fixture_sha256 = $null
        daemon_cleanup_ok = $false; fixture_cleanup_ok = $false; fixture_copy_removed = $false
        step_sequence = 0L; active_step = $null; last_step = $null
        stage_timings = [ordered]@{}; step_timings = [ordered]@{}
        deadline_crossings = [Collections.Generic.List[object]]::new(); deadline_diagnostics = $null
        slow_steps = [Collections.Generic.List[object]]::new(); step_journal = $null
    }
}

function Get-SteadyNow {
    param($Context)
    return [long](& $Context.IO.Now $Context)
}

function Get-SteadyBudgetSnapshot {
    param($Context, [long]$Now)
    $values = [ordered]@{}
    if ($null -ne $Context.Session) { $values.session = $Context.Plan.session_ms - ($Now - $Context.Session.StartMs) }
    if ($null -eq $Context.SteadyStartMs) { $values.setup = $Context.Plan.setup_ms - $Now }
    else { $values.run = $Context.SteadyStartMs + $Context.Plan.duration_ms + $Context.Plan.cleanup_ms - $Now }
    if ($null -ne $Context.CleanupStartMs) { $values.cleanup = $Context.Plan.cleanup_ms - ($Now - $Context.CleanupStartMs) }
    return $values
}

function Update-SteadyJournal {
    param($Context)
    if ($null -eq $Context.Journal) { return }
    $step = $Context.Result.active_step
    if ($null -eq $step) {
        $Context.Journal.Update('Idle', 'Idle', (Get-SteadyNow $Context), $Context.Result.step_sequence,
            $Context.Result.observations_completed, $Context.Result.sessions_attempted)
    }
    else {
        $Context.Journal.Update($step.stage, $step.step, $step.started_ms, $step.sequence,
            $Context.Result.observations_completed, $Context.Result.sessions_attempted)
    }
}

function Add-SteadyTiming {
    param($Map, [string]$Name, [long]$Elapsed, [bool]$Succeeded)
    if (-not $Map.Contains($Name)) {
        Require-AB ($Map.Count -lt 48) 'AB_STEADY_STEP_LIMIT'
        $Map[$Name] = [ordered]@{ calls=0; total_ms=0L; max_ms=0L; last_ms=0L; failures=0 }
    }
    $m = $Map[$Name]
    $m.calls++; $m.total_ms += $Elapsed; $m.max_ms = [Math]::Max($m.max_ms, $Elapsed); $m.last_ms = $Elapsed
    if (-not $Succeeded) { $m.failures++ }
}

function Invoke-SteadyStep {
    param($Context, [string]$Stage, [string]$Step, [scriptblock]$Operation)
    Require-AB ($Stage -cin @('Check','Call','Persist','Wait','Measure') -and $Step -cmatch '^[A-Za-z_.]{1,96}$') 'AB_STEADY_STEP_NAME'
    $previous = $Context.Result.active_step
    $start = Get-SteadyNow $Context
    $budgets = Get-SteadyBudgetSnapshot $Context $start
    $Context.Result.step_sequence++
    $Context.Result.active_step = [ordered]@{ stage=$Stage; step=$Step; started_ms=$start; sequence=$Context.Result.step_sequence }
    Update-SteadyJournal $Context
    $succeeded = $false
    try { & $Operation; $succeeded = $true }
    finally {
        $end = Get-SteadyNow $Context
        $elapsed = $end - $start
        $record = [ordered]@{ stage=$Stage; step=$Step; started_ms=$start; ended_ms=$end; elapsed_ms=$elapsed; succeeded=$succeeded }
        Add-SteadyTiming $Context.Result.step_timings $Step $elapsed $succeeded
        # 同类子步骤只进入 step_timings，避免把 Desktop 同时加进 Check 总计两次。
        if ($null -eq $previous -or $previous.stage -cne $Stage) {
            Add-SteadyTiming $Context.Result.stage_timings $Stage $elapsed $succeeded
        }
        $after = Get-SteadyBudgetSnapshot $Context $end
        foreach ($name in $budgets.Keys) {
            if ($after.Contains($name) -and $budgets[$name] -gt 0 -and $after[$name] -le 0 -and
                @($Context.Result.deadline_crossings | Where-Object { $_.budget -ceq $name }).Count -eq 0) {
                # 最内层先收尾，保留首次跨限的可观察步骤；这不是对底层根因的断言。
                $Context.Result.deadline_crossings.Add([ordered]@{
                    budget=$name; stage=$Stage; step=$Step; started_ms=$start; ended_ms=$end
                    elapsed_ms=$elapsed; remaining_before_ms=$budgets[$name]; remaining_after_ms=$after[$name]
                })
            }
        }
        if ($elapsed -ge 5000) {
            if ($Context.Result.slow_steps.Count -eq 32) { $Context.Result.slow_steps.RemoveAt(0) }
            $Context.Result.slow_steps.Add($record)
        }
        $Context.Result.last_step = $record
        $Context.Result.active_step = $previous
        Update-SteadyJournal $Context
    }
}

function Invoke-SteadyCheck {
    param($Context, [bool]$Cleanup)
    Invoke-SteadyStep $Context 'Check' 'Check' { & $Context.IO.Check $Context $Cleanup }
}

function Get-SteadyDeadlineMs {
    param($Context, [string]$Tool, [bool]$Cleanup)
    $now = Get-SteadyNow $Context
    if ($Cleanup) {
        $budgetKind = 'cleanup'
        if ($null -eq $Context.CleanupStartMs) { $Context.CleanupStartMs = $now }
        $left = $Context.Plan.cleanup_ms - ($now - $Context.CleanupStartMs)
    }
    else {
        $budgetKind = 'setup'
        $limit = $Context.Plan.setup_ms
        if ($null -ne $Context.SteadyStartMs) {
            $budgetKind = 'run'
            $limit = $Context.SteadyStartMs + $Context.Plan.duration_ms + $Context.Plan.cleanup_ms
        }
        $left = $limit - $now
        $sessionLeft = $Context.Plan.session_ms - ($now - $Context.Session.StartMs)
        if ($Tool -ceq 'get_window_state') { $sessionLeft -= $Context.Plan.call_ms }
        if ($sessionLeft -lt $left) { $budgetKind = 'session' }
        $left = [Math]::Min($left, $sessionLeft)
    }
    if ($left -le 0 -and $null -eq $Context.Result.deadline_diagnostics) {
        $Context.Result.deadline_diagnostics = [ordered]@{
            budget=$budgetKind; tool=$Tool; cleanup=$Cleanup; elapsed_ms=$now; remaining_ms=$left
            reserved_end_ms=$(if (-not $Cleanup -and $Tool -ceq 'get_window_state') { $Context.Plan.call_ms } else { 0 })
            budgets=Get-SteadyBudgetSnapshot $Context $now; last_step=$Context.Result.last_step
            observed_only=$true; root_cause='unproven'
        }
    }
    Require-AB ($left -gt 0) 'AB_STEADY_DEADLINE'
    return [int][Math]::Min($Context.Plan.call_ms, $left)
}

function Invoke-SteadyTool {
    param($Context, [string]$Tool, $Arguments, [bool]$Cleanup = $false)
    Require-AB ($Tool -cin $script:AllowedTools) 'AB_TOOL_NOT_ALLOWED'
    # 异常收尾仍核对 daemon，但锁屏/fixture 故障不能阻止释放既有 session。
    if ($Cleanup -and $null -eq $Context.CleanupStartMs) { $Context.CleanupStartMs = Get-SteadyNow $Context }
    Invoke-SteadyCheck $Context $Cleanup
    $deadline = Get-SteadyDeadlineMs $Context $Tool $Cleanup
    $reply = Invoke-SteadyStep $Context 'Call' ('Call.' + $Tool) { & $Context.IO.Call $Context $Tool $Arguments $deadline }
    Assert-ABNoImages $reply.Payload
    return $reply
}

function Write-SteadyPoint {
    param($Context, [string]$Phase)
    Invoke-SteadyCheck $Context $false
    $now = Get-SteadyNow $Context
    $Context.Result.phase = $Phase
    $Context.Result.elapsed_ms = $now
    $sample = [ordered]@{
        sequence = $Context.Result.samples_written + 1; phase = $Phase; elapsed_ms = $now
        steady_elapsed_ms = 0L; observations_completed = $Context.Result.observations_completed
        warmup_completed = $Context.Result.warmup_completed; session_ordinal = $Context.Result.sessions_attempted
        sessions_ended = $Context.Result.sessions_ended
        metrics = Invoke-SteadyStep $Context 'Measure' 'Measure' { & $Context.IO.Measure $Context }
    }
    if ($null -ne $Context.SteadyStartMs) {
        $sample.steady_elapsed_ms = $now - $Context.SteadyStartMs
        if ($Phase -cne 'after_end_session') { $Context.Result.steady_elapsed_ms = $sample.steady_elapsed_ms }
    }
    $Context.Result.samples_written++
    Invoke-SteadyStep $Context 'Persist' 'Persist' { & $Context.IO.Persist $Context $sample }
}

function Start-SteadySession {
    param($Context)
    Require-AB ($null -eq $Context.Session) 'AB_STEADY_SESSION_ACTIVE'
    Require-AB ($Context.Result.sessions_attempted -lt $Context.Plan.max_sessions) 'AB_STEADY_SESSION_BUDGET'
    $Context.Result.sessions_attempted++
    $Context.Session = [pscustomobject]@{
        Id = ('readonly-steady-' + [guid]::NewGuid().ToString('N'))
        StartMs = Get-SteadyNow $Context; Observations = 0; EndAttempted = $false
    }
    $Context.Seen.Clear()
    $reply = Invoke-SteadyTool $Context 'start_session' @{ session = $Context.Session.Id }
    Assert-ABSession $reply.Payload $Context.Session.Id $true
    $Context.Result.sessions_started++
    Write-SteadyPoint $Context 'session_started'
}

function End-SteadySession {
    param($Context, [bool]$Cleanup = $false)
    if ($null -eq $Context.Session -or $Context.Session.EndAttempted) { return }
    # 不重试结果不明的 end；本次直接失败，随后只定向清理持有的进程。
    $Context.Session.EndAttempted = $true
    $Context.Result.session_end_attempts++
    $reply = Invoke-SteadyTool $Context 'end_session' @{ session = $Context.Session.Id } $Cleanup
    Assert-ABSession $reply.Payload $Context.Session.Id $false
    $Context.Result.sessions_ended++
    $Context.Session = $null
    $Context.Seen.Clear()
}

function Wait-SteadyInterval {
    param($Context, [int]$Milliseconds)
    # 真机适配器把等待切成 250ms，并检查 cancel.request 与身份；自测只推进虚拟钟。
    Invoke-SteadyStep $Context 'Wait' 'Wait' { & $Context.IO.Wait $Context $Milliseconds }
}

function Invoke-SteadyObservations {
    param($Context)
    try {
        Write-SteadyPoint $Context 'before_warmup'
        $Context.NextSampleMs = (Get-SteadyNow $Context) + $Context.Plan.sample_ms
        while ($true) {
            $now = Get-SteadyNow $Context
            if ($null -ne $Context.SteadyStartMs -and $now - $Context.SteadyStartMs -ge $Context.Plan.duration_ms) { break }
            if ($null -ne $Context.Session -and
                ($Context.Session.Observations -ge $Context.Plan.session_observations -or
                $now - $Context.Session.StartMs -ge ($Context.Plan.session_ms - 2 * $Context.Plan.call_ms - $Context.Plan.interval_ms))) {
                Write-SteadyPoint $Context 'before_session_end'
                End-SteadySession $Context
                $Context.Result.rotations++
                Write-SteadyPoint $Context 'after_session_end'
            }
            if ($null -eq $Context.Session) { Start-SteadySession $Context }
            $request = New-ABStateArguments $Context.FixturePid $Context.WindowId $Context.Session.Id
            $reply = Invoke-SteadyTool $Context 'get_window_state' $request
            $counts = Assert-ABState $reply.Payload $Context.FixturePid $Context.WindowId $Context.Seen
            $Context.Session.Observations++
            $Context.Result.max_seen_snapshots = [Math]::Max($Context.Result.max_seen_snapshots, $Context.Seen.Count)
            $Context.Result.observed_elements_total += $counts.elements
            $Context.Result.observed_tree_chars_total += $counts.tree_chars
            if ($Context.Result.warmup_completed -lt $Context.Plan.warmup) {
                $Context.Result.warmup_completed++
                $Context.Result.warmup_elapsed_ms_total += $reply.ElapsedMs
                if ($Context.Result.warmup_completed -eq $Context.Plan.warmup) {
                    $Context.SteadyStartMs = Get-SteadyNow $Context
                    $Context.NextSampleMs = $Context.SteadyStartMs + $Context.Plan.sample_ms
                    Write-SteadyPoint $Context 'after_warmup'
                }
            }
            else {
                $Context.Result.observations_completed++
                $Context.Result.observation_elapsed_ms_total += $reply.ElapsedMs
            }
            $now = Get-SteadyNow $Context
            if ($now -ge $Context.NextSampleMs) {
                $phase = 'warmup'
                if ($null -ne $Context.SteadyStartMs) { $phase = 'observations' }
                Write-SteadyPoint $Context $phase
                $Context.NextSampleMs = $now + $Context.Plan.sample_ms
            }
            $reply = $null
            $delay = $Context.Plan.interval_ms
            if ($null -ne $Context.SteadyStartMs) {
                $remaining = $Context.Plan.duration_ms - ((Get-SteadyNow $Context) - $Context.SteadyStartMs)
                $delay = [int][Math]::Max(0, [Math]::Min($delay, $remaining))
            }
            Wait-SteadyInterval $Context $delay
        }
        $Context.Result.steady_elapsed_ms = (Get-SteadyNow $Context) - $Context.SteadyStartMs
        Require-AB ($Context.Result.observations_completed -gt 0) 'AB_STEADY_NO_OBSERVATIONS'
        Write-SteadyPoint $Context 'before_end_session'
        End-SteadySession $Context
        for ($i = 0; $i -lt $Context.Plan.post_end_samples; $i++) {
            Wait-SteadyInterval $Context $Context.Plan.post_end_interval_ms
            Write-SteadyPoint $Context 'after_end_session'
            $Context.Result.post_end_samples++
        }
    }
    finally {
        # 包括 start_session 响应不明、预算耗尽和人工取消；每次至多一次 end。
        if ($null -ne $Context.Session -and -not $Context.Session.EndAttempted) {
            try { End-SteadySession $Context $true }
            catch { $Context.Result.cleanup_failure_code = Get-ABFailureCode $_ }
        }
    }
}

function Assert-SteadyCompleted {
    param($Result)
    $p = $Result.plan
    Require-AB ($null -eq $Result.failure_code -and $null -eq $Result.cleanup_failure_code -and
        $Result.warmup_completed -eq $p.warmup -and $Result.observations_completed -gt 0 -and
        $Result.steady_elapsed_ms -ge $p.duration_ms -and
        $Result.sessions_started -gt 0 -and $Result.sessions_started -le $p.max_sessions -and
        $Result.sessions_attempted -eq $Result.sessions_started -and
        $Result.sessions_ended -eq $Result.sessions_started -and
        $Result.session_end_attempts -eq $Result.sessions_ended -and
        $Result.rotations -eq ($Result.sessions_ended - 1) -and
        $Result.rotations -ge $p.minimum_rotations -and
        $Result.max_seen_snapshots -gt 0 -and $Result.max_seen_snapshots -le $p.session_observations -and
        $Result.samples_written -ge (3 * $Result.sessions_started + 4) -and
        $Result.post_end_samples -eq $p.post_end_samples -and
        $Result.cli_attempts -eq (1 + 2 * $Result.sessions_started + $p.warmup + $Result.observations_completed) -and
        $Result.cli_exit_codes -eq $Result.cli_attempts -and $Result.cli_zero_exits -eq $Result.cli_attempts -and
        $Result.daemon_cleanup_ok -ceq $true -and (Test-ABInteger $Result.daemon_exit_code) -and
        $Result.fixture_cleanup_ok -ceq $true -and (Test-ABInteger $Result.fixture_exit_code) -and
        $Result.fixture_exit_code -eq 0 -and $Result.fixture_copy_removed -ceq $true -and
        (Test-ABInteger $Result.daemon_pid) -and $Result.daemon_pid -gt 0 -and
        (Test-ABInteger $Result.fixture_pid) -and $Result.fixture_pid -gt 0 -and
        (Test-ABInteger $Result.daemon_start_utc_ticks) -and $Result.daemon_start_utc_ticks -gt 0 -and
        (Test-ABInteger $Result.fixture_start_utc_ticks) -and $Result.fixture_start_utc_ticks -gt 0 -and
        $Result.candidate_sha256 -cmatch '^[0-9a-f]{64}$' -and
        $Result.fixture_sha256 -cmatch '^[0-9a-f]{64}$') 'AB_STEADY_INCOMPLETE'
}

function Write-SteadyJson {
    param([string]$Path, $Value)
    # 读者只会看到旧版或新版完整 JSON；崩溃留下 RUNNING 不可当作完成。
    $temporary = $Path + '.' + [guid]::NewGuid().ToString('N') + '.next'
    $json = $Value | ConvertTo-Json -Depth 10
    $stream = [IO.File]::Open($temporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        try {
            $bytes = [Text.UTF8Encoding]::new($false).GetBytes($json)
            $stream.Write($bytes, 0, $bytes.Length)
            $stream.Flush($true)
        }
        finally { $stream.Dispose() }
        # PS5.1 会把 string 参数位置的 $null 转成空串；备份路径必须是真正的 .NET null。
        if ([IO.File]::Exists($Path)) { [IO.File]::Replace($temporary, $Path, [NullString]::Value) }
        else { [IO.File]::Move($temporary, $Path) }
    }
    finally { if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) } }
}

function Assert-SteadyRunDirectory {
    param([string]$Path)
    Require-AB ([IO.Directory]::Exists($Path) -and
        ([IO.File]::GetAttributes($Path) -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'AB_STEADY_RUN_DIRECTORY'
}

function Assert-SteadyPathChain {
    param([string]$Path)
    $current = [IO.Path]::GetFullPath($Path)
    Require-AB ($current -match '^[A-Za-z]:[\\/]') 'AB_STEADY_OUTPUT_LOCAL_PATH'
    while (-not [string]::IsNullOrEmpty($current)) {
        if ([IO.File]::Exists($current) -or [IO.Directory]::Exists($current)) {
            Require-AB ([IO.Directory]::Exists($current) -and
                ([IO.File]::GetAttributes($current) -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'AB_STEADY_OUTPUT_LINK'
        }
        $parent = [IO.Directory]::GetParent($current)
        if ($null -eq $parent) { break }
        $current = $parent.FullName
    }
}

function Resolve-SteadyOutputRoot {
    param([AllowEmptyString()][string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) {
        $temp = [IO.Path]::GetTempPath()
        # 默认不能静默回到机械盘；TEMP 被重定向到其他盘时要求显式选择 OutputRoot。
        Require-AB ([string]::Equals([IO.Path]::GetPathRoot($temp),
            [IO.Path]::GetPathRoot([Environment]::GetFolderPath('Windows')), [StringComparison]::OrdinalIgnoreCase)) 'AB_STEADY_DEFAULT_TEMP_DRIVE'
        $Value = Join-Path $temp 'CortexReadonlySteady'
    }
    Require-AB ([IO.Path]::IsPathRooted($Value)) 'AB_STEADY_OUTPUT_ABSOLUTE'
    $full = [IO.Path]::GetFullPath($Value)
    Assert-SteadyPathChain $full
    return $full
}

function New-SteadyRunDirectory {
    param([string]$Parent, [string]$Name)
    Require-AB ($Name -cmatch '^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$') 'AB_STEADY_RUN_NAME'
    Assert-SteadyPathChain $Parent
    $null = [IO.Directory]::CreateDirectory($Parent)
    Assert-SteadyPathChain $Parent
    $root = Join-Path $Parent ('steady-' + $Name)
    Require-AB (-not [IO.Directory]::Exists($root) -and -not [IO.File]::Exists($root)) 'AB_OUTPUT_EXISTS'
    # 独立数据目录仅当前 Windows 身份与 SYSTEM 可读写，未触碰用户/机器配置。
    $security = [Security.AccessControl.DirectorySecurity]::new()
    $security.SetAccessRuleProtection($true, $false)
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    try {
        $security.SetOwner($identity.User)
        foreach ($sid in @($identity.User, [Security.Principal.SecurityIdentifier]::new('S-1-5-18'))) {
            $rule = [Security.AccessControl.FileSystemAccessRule]::new($sid,
                [Security.AccessControl.FileSystemRights]::FullControl,
                [Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit',
                [Security.AccessControl.PropagationFlags]::None, [Security.AccessControl.AccessControlType]::Allow)
            $security.AddAccessRule($rule)
        }
        $null = [IO.Directory]::CreateDirectory($root, $security)
    }
    finally { $identity.Dispose() }
    Assert-SteadyPathChain $root
    return $root
}

if ($steadyMode -ceq 'SourceOnly') { return }
if ($steadyMode -ceq 'SelfTest') {
    . (Join-Path $PSScriptRoot 'steady-selftest.ps1')
    Invoke-SteadySelfTests
    return
}

$steadyPlan = New-SteadyPlan $steadyOptions
$steadyResult = New-SteadyResult $steadyPlan
# 仅关联离线证据，不参与授权、清理所有权或 PASS 判定；缺失时保留 null。
$steadyResult.run_name = $steadyOptions.RunName
$steadyResult.harness_pid = $PID
$steadyResult.owner_run_id = [Environment]::GetEnvironmentVariable('CORTEX_CUA_STEADY_RUN_ID')
$steadyPins = [Collections.Generic.List[object]]::new()
$steadyFixture = $null; $steadyDaemon = $null; $steadyRoot = $null; $steadyCopy = $null
$steadySamples = $null; $steadyContext = $null; $steadyLease = $null
$steadyJournal = $null
try {
    Require-AB ($env:OS -ceq 'Windows_NT') 'AB_WINDOWS_REQUIRED'
    $runs = Resolve-SteadyOutputRoot $steadyOptions.OutputRoot
    $requestedRoot = New-SteadyRunDirectory $runs $steadyOptions.RunName
    # 取得独占创建凭据后才允许 finally 写报告；重复 RunName 不能覆写旧结果。
    $steadyLease = [IO.File]::Open((Join-Path $requestedRoot 'run.lock'), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $steadyRoot = $requestedRoot
    $steadyResult.output_directory = $steadyRoot
    Write-Output ('STEADY_RUNNING run=steady-' + $steadyOptions.RunName + ' output_directory=' + $steadyRoot + ' scope=' + $script:SteadyScope)
    Write-SteadyJson (Join-Path $steadyRoot 'progress.json') $steadyResult
    $candidate = Open-ABPinnedFile $steadyOptions.CandidatePath $steadyOptions.CandidateSha
    $steadyPins.Add($candidate)
    $fixtureSource = Open-ABPinnedFile $steadyOptions.FixturePath $steadyOptions.FixtureSha
    $steadyPins.Add($fixtureSource)
    Require-AB (-not [string]::Equals($candidate.Path, $fixtureSource.Path, [StringComparison]::OrdinalIgnoreCase)) 'AB_DRIVER_PATHS_EQUAL'
    $steadyCopy = Join-Path $steadyRoot ('fixture-' + [guid]::NewGuid().ToString('N') + '.exe')
    $copyStream = [IO.File]::Open($steadyCopy, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $fixtureSource.Stream.CopyTo($copyStream) } finally { $copyStream.Dispose() }
    $fixturePin = Open-ABPinnedFile $steadyCopy $steadyOptions.FixtureSha
    $steadyPins.Add($fixturePin)
    $steadyResult.candidate_sha256 = $candidate.Sha256; $steadyResult.fixture_sha256 = $fixturePin.Sha256
    $manifest = New-SteadyManifest $steadyCopy $steadyPlan
    Assert-SteadyManifest $manifest $steadyCopy $steadyPlan
    $manifestPath = Join-Path $steadyRoot 'manifest.json'
    Write-SteadyJson $manifestPath $manifest
    $environment = New-ABEnvironment (Join-Path $steadyRoot 'driver-home')
    $socket = '\\.\pipe\cortex-readonly-steady-' + [guid]::NewGuid().ToString('N')
    $pidPath = Join-Path $steadyRoot 'daemon.pid'
    Initialize-ABNative
    Require-AB ([CuaReadonlyAB.Windows]::InteractiveDesktop()) 'AB_DESKTOP_NOT_INTERACTIVE'
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $steadyFixture = Start-ABOwnedProcess $steadyCopy @('--no-secure-field', '--exit-timeout-ms',
        [string](($steadyPlan.manifest_seconds + 60) * 1000)) @{} 0 'fixture'
    $steadyFixture.Process.StandardInput.Close()
    $windowId = 0L
    while ($clock.ElapsedMilliseconds -lt 15000 -and $windowId -eq 0) {
        Assert-ABOwnedProcess $steadyFixture
        $handles = [CuaReadonlyAB.Windows]::VisibleWindows($steadyFixture.Process)
        Require-AB ($handles.Length -le 1) 'AB_FIXTURE_WINDOW_COUNT'
        if ($handles.Length -eq 1) { $windowId = $handles[0] }
        else { Start-Sleep -Milliseconds 50 }
    }
    Require-AB ($windowId -ne 0) 'AB_FIXTURE_WINDOW_TIMEOUT'
    Assert-ABFixtureWindow $steadyFixture $windowId
    $steadyDaemon = Start-ABOwnedProcess $candidate.Path @('serve', '--permission-mode', 'bounded',
        '--capability-manifest', $manifestPath, '--approve-capability-manifest', '--socket', $socket,
        '--pid-file', $pidPath, '--no-overlay') $environment 0 'daemon'
    $steadyDaemon.Process.StandardInput.Close()
    $startup = [Diagnostics.Stopwatch]::StartNew()
    $ready = $false
    while ($startup.ElapsedMilliseconds -lt 15000 -and -not $ready) {
        Assert-ABOwnedProcess $steadyDaemon
        $ready = [CuaReadonlyAB.Windows]::PipeReady($socket)
        if (-not $ready) { Start-Sleep -Milliseconds 50 }
    }
    Require-AB $ready 'AB_DAEMON_NOT_READY'
    $status = Invoke-ABCLI $candidate @('status', '--socket', $socket, '--pid-file', $pidPath) '' $environment $steadyPlan.call_ms $steadyResult
    Assert-ABStatus $status.Text
    $status = $null
    $steadyResult.daemon_pid = $steadyDaemon.Pid; $steadyResult.daemon_start_utc_ticks = $steadyDaemon.StartTicks
    $steadyResult.fixture_pid = $steadyFixture.Pid; $steadyResult.fixture_start_utc_ticks = $steadyFixture.StartTicks
    $sampleStream = [IO.File]::Open((Join-Path $steadyRoot 'samples.jsonl'), [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $steadySamples = [IO.StreamWriter]::new($sampleStream, [Text.UTF8Encoding]::new($false))
    $steadyJournal = [CuaReadonlyAB.StepJournal]::new((Join-Path $steadyRoot 'steps-progress.json'), $clock)
    $steadyContext = [pscustomobject]@{
        Plan = $steadyPlan; Result = $steadyResult; Driver = $candidate; Daemon = $steadyDaemon
        Fixture = $steadyFixture; FixturePid = $steadyFixture.Pid; WindowId = $windowId
        Root = $steadyRoot; Environment = $environment; Socket = $socket; Clock = $clock
        Writer = $steadySamples; SampleStream = $sampleStream; Journal = $steadyJournal
        Session = $null; SteadyStartMs = $null; CleanupStartMs = $null; NextSampleMs = 0L
        Seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        IO = @{
            Now = { param($c) return $c.Clock.ElapsedMilliseconds }
            Check = {
                param($c, $cleanup)
                $trace = { param($name, $operation) Invoke-SteadyStep $c 'Check' ('Check.' + $name) $operation }
                Assert-ABOwnedProcess $c.Daemon $trace
                Require-AB ($c.Daemon.Pid -eq $c.Result.daemon_pid -and
                    $c.Daemon.StartTicks -eq $c.Result.daemon_start_utc_ticks) 'AB_PROCESS_IDENTITY'
                if (-not $cleanup) {
                    Invoke-SteadyStep $c 'Check' 'Check.Cancel' {
                        Require-AB (-not [IO.File]::Exists((Join-Path $c.Root 'cancel.request'))) 'AB_STEADY_CANCELLED'
                    }
                    Assert-ABFixtureWindow $c.Fixture $c.WindowId $trace
                    Require-AB ($c.Fixture.Pid -eq $c.Result.fixture_pid -and
                        $c.Fixture.StartTicks -eq $c.Result.fixture_start_utc_ticks) 'AB_PROCESS_IDENTITY'
                }
            }
            Call = {
                param($c, $tool, $arguments, $deadline)
                $json = $arguments | ConvertTo-Json -Compress -Depth 12
                $reply = Invoke-ABCLI $c.Driver @('call', '--socket', $c.Socket, $tool) $json $c.Environment $deadline $c.Result
                try { $payload = ConvertFrom-Json -InputObject $reply.Text -ErrorAction Stop }
                catch { throw [InvalidOperationException]::new('AB_CLI_JSON') }
                return [pscustomobject]@{ Payload = $payload; ElapsedMs = $reply.ElapsedMs }
            }
            Measure = {
                param($c)
                $trace = { param($name, $operation) Invoke-SteadyStep $c 'Measure' ('Measure.' + $name) $operation }
                return [ordered]@{ driver = Get-ABProcessMetrics $c.Daemon $trace; fixture = Get-ABProcessMetrics $c.Fixture $trace }
            }
            Persist = {
                param($c, $sample)
                Invoke-SteadyStep $c 'Persist' 'Persist.Samples' {
                    $c.Writer.WriteLine(($sample | ConvertTo-Json -Compress -Depth 6))
                    $c.Writer.Flush(); $c.SampleStream.Flush($true)
                }
                Invoke-SteadyStep $c 'Persist' 'Persist.Progress' { Write-SteadyJson (Join-Path $c.Root 'progress.json') $c.Result }
            }
            Wait = {
                param($c, $milliseconds)
                $until = $c.Clock.ElapsedMilliseconds + $milliseconds
                while ($c.Clock.ElapsedMilliseconds -lt $until) {
                    Invoke-SteadyCheck $c $false
                    $left = $until - $c.Clock.ElapsedMilliseconds
                    if ($left -gt 0) { Start-Sleep -Milliseconds ([int][Math]::Min(250, $left)) }
                }
            }
        }
    }
    Invoke-SteadyObservations $steadyContext
}
catch { $steadyResult.failure_code = Get-ABFailureCode $_ }
finally {
    if ($null -ne $steadyJournal) {
        $steadyJournal.Dispose()
        $steadyResult.step_journal = [ordered]@{ writes=$steadyJournal.Writes; errors=$steadyJournal.Errors; last_io_ms=$steadyJournal.LastIoMs; drained=$steadyJournal.Drained }
    }
    $steadyResult.phase = 'cleanup'
    if ($null -ne $steadyRoot) {
        try { Write-SteadyJson (Join-Path $steadyRoot 'progress.json') $steadyResult }
        catch { $steadyResult.cleanup_failure_code = 'AB_STEADY_REPORT_WRITE' }
    }
    $steadyResult.daemon_cleanup_ok = Stop-ABOwnedProcess $steadyDaemon $false
    if ($null -ne $steadyDaemon) { $steadyResult.daemon_exit_code = $steadyDaemon.ExitCode }
    $steadyResult.fixture_cleanup_ok = Stop-ABOwnedProcess $steadyFixture $true
    if ($null -ne $steadyFixture) { $steadyResult.fixture_exit_code = $steadyFixture.ExitCode }
    foreach ($pin in $steadyPins) { $pin.Stream.Dispose() }
    if ($null -ne $steadySamples) { $steadySamples.Dispose() }
    # 只删本次命名的单个副本；driver home 可能含诊断产物，保留且记录，不递归删除。
    if ($null -ne $steadyCopy -and $steadyResult.fixture_cleanup_ok) {
        try {
            Assert-SteadyRunDirectory $steadyRoot
            Require-AB (([IO.File]::GetAttributes($steadyCopy) -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'AB_FILE_REPARSE'
            [IO.File]::Delete($steadyCopy)
            $steadyResult.fixture_copy_removed = -not [IO.File]::Exists($steadyCopy)
        }
        catch { $steadyResult.cleanup_failure_code = 'AB_STEADY_FIXTURE_COPY_CLEANUP' }
    }
    try { Assert-SteadyCompleted $steadyResult; $steadyResult.status = 'PASS' }
    catch {
        $steadyResult.status = 'FAIL'
        if ($null -eq $steadyResult.failure_code) { $steadyResult.failure_code = Get-ABFailureCode $_ }
    }
    $steadyResult.phase = 'completed'
    if ($null -ne $steadyRoot) {
        try {
            Write-SteadyJson (Join-Path $steadyRoot 'summary.json') $steadyResult
            Write-SteadyJson (Join-Path $steadyRoot 'progress.json') $steadyResult
        }
        finally { if ($null -ne $steadyLease) { $steadyLease.Dispose() } }
    }
}
if ($steadyResult.status -cne 'PASS') { Write-Output ('STEADY_FAIL code=' + $steadyResult.failure_code + ' scope=' + $script:SteadyScope); exit 1 }
Write-Output ('STEADY_PASS scope=' + $script:SteadyScope)
