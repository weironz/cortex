#requires -Version 5.1
# 内存用例由 -SelfTest 运行；真实 JSON 文件回归仅由验证器显式调用。
function Invoke-SteadyJsonFileSelfTest {
    # 此项由源码验证器单独调用；仅在系统临时目录的唯一私有子目录测试真实替换。
    $checks = 0
    $parent = Resolve-SteadyOutputRoot ''
    $root = New-SteadyRunDirectory $parent ('json-file-selftest-' + [guid]::NewGuid().ToString('N'))
    Require-AB ($root.StartsWith($parent + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) 'AB_JSON_FILE_SELFTEST_PRIVATE_PATH'
    $checks++
    $path = Join-Path $root 'progress.json'
    try {
        Write-SteadyJson $path @{ sequence=1; status='RUNNING'; padding=('x' * 256) }
        $first = [IO.File]::ReadAllText($path, [Text.Encoding]::UTF8) | ConvertFrom-Json
        Require-AB ($first.sequence -eq 1 -and $first.status -ceq 'RUNNING' -and $first.padding.Length -eq 256) 'AB_JSON_FILE_SELFTEST_FIRST_WRITE'
        $checks++
        Write-SteadyJson $path @{ sequence=2; status='FAIL'; failure_code='AB_SYNTHETIC_ORIGINAL_FAILURE' }
        $second = [IO.File]::ReadAllText($path, [Text.Encoding]::UTF8) | ConvertFrom-Json
        Require-AB ($second.sequence -eq 2 -and $second.status -ceq 'FAIL' -and
            $second.failure_code -ceq 'AB_SYNTHETIC_ORIGINAL_FAILURE' -and
            $null -eq (Get-ABField $second 'padding')) 'AB_JSON_FILE_SELFTEST_REPLACE'
        $checks++
        $files = [IO.Directory]::GetFileSystemEntries($root)
        Require-AB ($files.Length -eq 1 -and $files[0] -ceq $path) 'AB_JSON_FILE_SELFTEST_TEMP_LEFTOVER'
        $checks++
    }
    finally {
        # 不递归删除；只清理本次命名的单个 JSON 和已确认的私有空目录。
        Assert-SteadyRunDirectory $parent
        Assert-SteadyRunDirectory $root
        if ([IO.File]::Exists($path)) {
            Require-AB (([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'AB_FILE_REPARSE'
            [IO.File]::Delete($path)
        }
        [IO.Directory]::Delete($root, $false)
    }
    Require-AB (-not [IO.Directory]::Exists($root)) 'AB_JSON_FILE_SELFTEST_CLEANUP'
    $checks++
    Write-Output ('STEADY_JSON_FILE_SELFTEST_PASS checks=' + $checks + ' writes=2 replace=verified cleanup=verified gui_driver_started=0')
}

function Invoke-SteadyOutputSelfTests {
    $checks = 0
    $parent = Resolve-SteadyOutputRoot ''
    Require-AB ([string]::Equals([IO.Path]::GetPathRoot($parent), [IO.Path]::GetPathRoot([Environment]::GetFolderPath('Windows')),
        [StringComparison]::OrdinalIgnoreCase)) 'AB_OUTPUT_SELFTEST_SYSTEM_TEMP'
    $checks++
    foreach ($value in @('relative-output', '\\server\share\output')) {
        $rejected = $false
        try { $null = Resolve-SteadyOutputRoot $value }
        catch { Require-AB ($_.Exception.Message -in @('AB_STEADY_OUTPUT_ABSOLUTE','AB_STEADY_OUTPUT_LOCAL_PATH')) 'AB_OUTPUT_SELFTEST_WRONG_REJECTION'; $rejected=$true }
        Require-AB $rejected 'AB_OUTPUT_SELFTEST_UNSAFE_PATH'
        $checks++
    }
    $name = 'output-selftest-' + [guid]::NewGuid().ToString('N')
    $root = New-SteadyRunDirectory $parent $name
    $journal = $null; $junction = Join-Path $root 'link'; $target = Join-Path $root 'target'
    $journalPath = Join-Path $root 'steps-progress.json'
    try {
        $rejected = $false
        try { $null = New-SteadyRunDirectory $parent $name }
        catch { Require-AB ($_.Exception.Message -ceq 'AB_OUTPUT_EXISTS') 'AB_OUTPUT_SELFTEST_WRONG_REJECTION'; $rejected=$true }
        Require-AB $rejected 'AB_OUTPUT_SELFTEST_OVERWRITE'
        $checks++
        $acl = [IO.Directory]::GetAccessControl($root)
        Require-AB $acl.AreAccessRulesProtected 'AB_OUTPUT_SELFTEST_PRIVATE_ACL'
        $checks++
        $null = [IO.Directory]::CreateDirectory($target)
        $null = New-Item -ItemType Junction -Path $junction -Target $target
        $rejected = $false
        try { Assert-SteadyPathChain (Join-Path $junction 'child') }
        catch { Require-AB ($_.Exception.Message -ceq 'AB_STEADY_OUTPUT_LINK') 'AB_OUTPUT_SELFTEST_WRONG_REJECTION'; $rejected=$true }
        Require-AB $rejected 'AB_OUTPUT_SELFTEST_LINK'
        $checks++
        $clock = [Diagnostics.Stopwatch]::StartNew()
        $journal = [CuaReadonlyAB.StepJournal]::new($journalPath, $clock)
        $journal.Update('Check', 'Check.Desktop', 0, 1, 2, 3)
        while ($journal.Writes -lt 2 -and $clock.ElapsedMilliseconds -lt 4500) { Start-Sleep -Milliseconds 10 }
        $journal.Dispose()
        $saved = [IO.File]::ReadAllText($journalPath, [Text.Encoding]::UTF8) | ConvertFrom-Json
        Require-AB ($journal.Writes -ge 2 -and $journal.Errors -eq 0 -and $journal.Drained -and
            $saved.stage -ceq 'Check' -and $saved.step -ceq 'Check.Desktop' -and $saved.observations_completed -eq 2) 'AB_OUTPUT_SELFTEST_JOURNAL'
        $checks++
    }
    finally {
        if ($null -ne $journal) { $journal.Dispose() }
        Assert-SteadyPathChain $root
        if ([IO.Directory]::Exists($junction)) { [IO.Directory]::Delete($junction, $false) }
        if ([IO.Directory]::Exists($target)) { [IO.Directory]::Delete($target, $false) }
        foreach ($file in @($journalPath, ($journalPath + '.next'))) { if ([IO.File]::Exists($file)) { [IO.File]::Delete($file) } }
        [IO.Directory]::Delete($root, $false)
    }
    Write-Output ('STEADY_OUTPUT_SELFTEST_PASS checks=' + $checks + ' system_temp=true overwrite=denied junction=denied private_acl=true journal=background gui_driver_started=0')
}

function New-SteadyMemoryContext {
    param([string]$Failure = '', [int]$MaxSessionCount = 32, $Plan = $null)
    $options = @{
        DurationSeconds = 60; Warmup = 2; SessionSeconds = 60; SessionObservationLimit = 3
        MaxSessions = $MaxSessionCount; IntervalMs = 1000; CallMs = 1000; SampleSeconds = 5
    }
    if ($null -eq $Plan) { $Plan = New-SteadyPlan $options }
    $plan = $Plan
    $result = New-SteadyResult $plan
    # 真机有且只有一次 status，之后每个工具调用都由同一引擎记账。
    $result.cli_attempts = 1; $result.cli_exit_codes = 1; $result.cli_zero_exits = 1
    $result.daemon_pid = 321; $result.daemon_start_utc_ticks = 100L
    $result.fixture_pid = 123; $result.fixture_start_utc_ticks = 200L
    $result.candidate_sha256 = 'a' * 64; $result.fixture_sha256 = 'b' * 64
    $memory = @{
        Now = 0L; Failure = $Failure; Sequence = 0; Checks = 0; CallAdvanceMs = 200; SlowInjected = $false
        Calls = [Collections.Generic.List[object]]::new()
        Samples = [Collections.Generic.List[object]]::new()
        Active = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        Started = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    }
    return [pscustomobject]@{
        Plan = $plan; Result = $result; FixturePid = 123; WindowId = 456L; Journal = $null
        Session = $null; SteadyStartMs = $null; CleanupStartMs = $null; NextSampleMs = 0L
        Seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        Memory = $memory
        IO = @{
            Now = { param($c) return $c.Memory.Now }
            Check = {
                param($c, $cleanup)
                $c.Memory.Checks++
                if (-not $cleanup -and $c.Memory.Failure -ceq 'slow_check' -and
                    $c.Memory.Sequence -ge 4 -and -not $c.Memory.SlowInjected) {
                    $c.Memory.SlowInjected = $true; $c.Memory.Now += 70000
                }
                if (-not $cleanup -and $c.Memory.Failure -ceq 'cancel' -and $c.Memory.Sequence -ge 4) { throw 'AB_STEADY_CANCELLED' }
                if (-not $cleanup -and $c.Memory.Failure -ceq 'identity' -and $c.Memory.Sequence -ge 4) { throw 'AB_PROCESS_IDENTITY' }
            }
            Call = {
                param($c, $tool, $arguments, $deadline)
                Require-AB ($deadline -gt 0 -and $deadline -le $c.Plan.call_ms) 'AB_SELFTEST_UNBOUNDED_RPC'
                $c.Result.cli_attempts++
                $c.Memory.Calls.Add(@{ tool = $tool; session = $arguments.session; deadline = $deadline; at_ms = $c.Memory.Now })
                $c.Memory.Now += $c.Memory.CallAdvanceMs
                if ($c.Memory.Failure -ceq $tool -or ($c.Memory.Failure -ceq 'cli' -and $tool -ceq 'get_window_state')) { throw 'AB_CLI_TIMEOUT' }
                $c.Result.cli_exit_codes++; $c.Result.cli_zero_exits++
                switch ($tool) {
                    'start_session' {
                        Require-AB ($c.Memory.Active.Count -eq 0 -and $c.Memory.Started.Add($arguments.session)) 'AB_SELFTEST_SESSION_REUSE'
                        $null = $c.Memory.Active.Add($arguments.session)
                        $payload = @{ session = $arguments.session; active = $true }
                    }
                    'end_session' {
                        if ($c.Memory.Failure -cne 'start_session') {
                            Require-AB ($c.Memory.Active.Remove($arguments.session)) 'AB_SELFTEST_END_UNKNOWN_SESSION'
                        }
                        $payload = @{ session = $arguments.session; active = $false }
                    }
                    'get_window_state' {
                        Require-AB ($c.Memory.Active.Contains($arguments.session) -and
                            $arguments.pid -eq 123 -and $arguments.window_id -eq 456 -and
                            $arguments.include_screenshot -ceq $false -and $null -eq $arguments.since -and
                            $arguments.tree_format -ceq 'both') 'AB_SELFTEST_UNSAFE_REQUEST'
                        $c.Memory.Sequence++
                        $snapshot = 'memory-' + $c.Memory.Sequence
                        if ($c.Memory.Failure -ceq 'replay') { $snapshot = 'memory-repeated' }
                        $payload = @{
                            pid = 123; window_id = 456L; window_title = $script:FixtureTitle
                            snapshot_id = $snapshot; tree_format = 'both'; tree_markdown = 'private-memory-only'
                            elements_complete = $true; truncated = $false; total_element_count = 1; returned_element_count = 1
                            elements = @(@{ element_token = ($snapshot + ':0') })
                        }
                        if ($c.Memory.Failure -ceq 'image') { $payload.screenshot_png_b64 = 'private-memory-only' }
                        if ($c.Memory.Failure -ceq 'wrong_pid') { $payload.pid = 999 }
                    }
                    default { throw 'AB_SELFTEST_UNKNOWN_TOOL' }
                }
                return [pscustomobject]@{ Payload = $payload; ElapsedMs = [double]$c.Memory.CallAdvanceMs }
            }
            Wait = { param($c, $milliseconds) $c.Memory.Now += $milliseconds }
            Measure = {
                param($c)
                return @{ driver = @{ pid = 321; private_bytes = 100 }; fixture = @{ pid = 123; private_bytes = 200 } }
            }
            Persist = {
                param($c, $sample)
                if ($c.Memory.Failure -ceq 'slow_persist' -and $c.Memory.Sequence -ge 4 -and -not $c.Memory.SlowInjected) {
                    $c.Memory.SlowInjected = $true; $c.Memory.Now += 70000
                }
                # 走相同序列化深度，防止以后把 AX/session/原始错误带进输出。
                $json = $sample | ConvertTo-Json -Depth 6 -Compress
                Require-AB ($json -notmatch 'private-memory-only|memory-\d|readonly-steady-|snapshot_id|element_token') 'AB_SELFTEST_PAYLOAD_PERSISTED'
                $c.Memory.Samples.Add(($json | ConvertFrom-Json))
            }
        }
    }
}

function Invoke-SteadyDiagnosticsSelfTests {
    $checks = 0
    foreach ($kind in @('slow_check', 'slow_persist')) {
        $c = New-SteadyMemoryContext $kind
        $c.Plan.duration_ms = 180000; $c.Plan.session_observations = 500
        $rejected = $false
        try { Invoke-SteadyObservations $c }
        catch { Require-AB ($_.Exception.Message -ceq 'AB_STEADY_DEADLINE') 'AB_DIAGNOSTIC_WRONG_FAILURE'; $rejected = $true }
        Require-AB $rejected 'AB_DIAGNOSTIC_DEADLINE_NOT_REJECTED'
        $checks++
        $stage = 'Check'
        if ($kind -ceq 'slow_persist') { $stage = 'Persist' }
        $crossing = @($c.Result.deadline_crossings | Where-Object { $_.budget -ceq 'session' })
        Require-AB ($crossing.Count -eq 1 -and $crossing[0].stage -ceq $stage -and
            $crossing[0].elapsed_ms -ge 70000 -and $c.Result.stage_timings[$stage].max_ms -ge 70000) 'AB_DIAGNOSTIC_TIMER_SOURCE'
        $checks++
        Require-AB ($c.Result.cli_attempts -eq $c.Result.cli_zero_exits -and
            $c.Result.stage_timings.Call.max_ms -le $c.Plan.call_ms -and
            $c.Result.deadline_diagnostics.budget -ceq 'session' -and
            $c.Result.deadline_diagnostics.root_cause -ceq 'unproven') 'AB_DIAGNOSTIC_RPC_MISATTRIBUTED'
        $checks++
    }
    $c = New-SteadyMemoryContext
    $c.Session = [pscustomobject]@{ StartMs=0L }
    Invoke-SteadyStep $c 'Check' 'Check' { Invoke-SteadyStep $c 'Check' 'Check.Desktop' { $c.Memory.Now += 70000 } }
    Require-AB ($c.Result.deadline_crossings[0].step -ceq 'Check.Desktop' -and
        $c.Result.stage_timings.Check.total_ms -eq 70000 -and $c.Result.step_timings['Check.Desktop'].total_ms -eq 70000) 'AB_DIAGNOSTIC_NESTED_TIMER'
    $checks++
    $c = New-SteadyMemoryContext
    $c.Session = [pscustomobject]@{ StartMs=0L }
    $process = [pscustomobject]@{ HasExited=$false; Id=123; StartTime=([datetime]'2026-01-01T00:00:00Z') }
    $process | Add-Member -MemberType ScriptMethod -Name Refresh -Value {}
    $process | Add-Member -MemberType ScriptProperty -Name MainModule -Value {
        $c.Memory.Now += 70000
        return [pscustomobject]@{ FileName='C:\synthetic\only.exe' }
    }.GetNewClosure()
    $owner = [pscustomobject]@{ Process=$process; Pid=123; StartTicks=$process.StartTime.ToUniversalTime().Ticks; Path='C:\synthetic\only.exe'; Role='synthetic' }
    Invoke-SteadyStep $c 'Check' 'Check' {
        $trace = { param($name, $operation) Invoke-SteadyStep $c 'Check' ('Check.' + $name) $operation }
        Assert-ABOwnedProcess $owner $trace
    }
    Require-AB ($c.Result.deadline_crossings[0].step -ceq 'Check.synthetic.MainModule' -and
        $c.Result.step_timings['Check.synthetic.Identity'].max_ms -eq 0) 'AB_DIAGNOSTIC_MAINMODULE_TRACE'
    $checks++
    $c = New-SteadyMemoryContext
    for ($i = 0; $i -lt 40; $i++) { Invoke-SteadyStep $c 'Wait' 'Wait' { $c.Memory.Now += 6000 } }
    Require-AB ($c.Result.slow_steps.Count -eq 32 -and $c.Result.stage_timings.Wait.calls -eq 40) 'AB_DIAGNOSTIC_BOUNDED_HISTORY'
    $checks++
    $c = New-SteadyMemoryContext
    $c.Plan.duration_ms = 180000; $c.Plan.session_ms = 80000; $c.Plan.session_observations = 500; $c.Plan.minimum_rotations = 2
    Invoke-SteadyObservations $c
    Require-AB ($c.Result.rotations -ge 2 -and $c.Result.sessions_started -ge 3 -and $c.Memory.Active.Count -eq 0) 'AB_DIAGNOSTIC_PROBE_ROTATIONS'
    $checks++
    foreach ($name in @('Check','Call','Persist','Wait','Measure')) {
        Require-AB ($c.Result.stage_timings.Contains($name) -and $c.Result.stage_timings[$name].calls -gt 0) 'AB_DIAGNOSTIC_MISSING_TIMER'
        $checks++
    }
    Require-AB ($null -eq $c.Result.run_name -and $null -eq $c.Result.harness_pid -and $null -eq $c.Result.owner_run_id) 'AB_DIAGNOSTIC_CORRELATION_IS_NOT_AUTHORITY'
    $checks++
    Write-Output ('STEADY_DIAGNOSTICS_SELFTEST_PASS checks=' + $checks + ' slow_check=deadline slow_persist=deadline root_cause=unproven gui_driver_started=0')
}

function Invoke-SteadySelfTests {
    $checks = 0
    function Expect-SteadyReject {
        param([scriptblock]$Action, [string]$Code)
        $rejected = $false
        try { $null = & $Action }
        catch {
            Require-AB ($_.Exception.Message -ceq $Code) 'AB_SELFTEST_WRONG_REJECTION'
            $rejected = $true
        }
        Require-AB $rejected 'AB_SELFTEST_EXPECTED_REJECTION'
    }
    $context = New-SteadyMemoryContext
    Invoke-SteadyObservations $context
    Require-AB ($context.Result.steady_elapsed_ms -ge 60000 -and $context.Result.sessions_started -gt 1 -and
        $context.Memory.Active.Count -eq 0 -and $context.Seen.Count -eq 0 -and
        $context.Result.max_seen_snapshots -eq 3 -and $context.Result.post_end_samples -eq 3) 'AB_SELFTEST_STEADY_LOOP'
    $checks++
    $phases = @($context.Memory.Samples | ForEach-Object { $_.phase })
    foreach ($phase in @('before_warmup', 'after_warmup', 'observations', 'before_session_end', 'after_session_end', 'before_end_session', 'after_end_session')) {
        Require-AB ($phase -cin $phases) 'AB_SELFTEST_SAMPLE_MISSING'
        $checks++
    }
    # 完成断言必须等待进程与副本清理，不能在满 4h 时提前 PASS。
    Expect-SteadyReject { Assert-SteadyCompleted $context.Result } 'AB_STEADY_INCOMPLETE'
    $context.Result.daemon_cleanup_ok = $true; $context.Result.daemon_exit_code = -1
    $context.Result.fixture_cleanup_ok = $true; $context.Result.fixture_exit_code = 0
    $context.Result.fixture_copy_removed = $true
    Assert-SteadyCompleted $context.Result
    $checks += 2
    foreach ($field in @('warmup_completed', 'observations_completed', 'steady_elapsed_ms', 'sessions_started',
            'sessions_ended', 'session_end_attempts', 'rotations', 'cli_attempts', 'cli_exit_codes', 'cli_zero_exits',
            'post_end_samples', 'daemon_cleanup_ok', 'daemon_exit_code', 'fixture_cleanup_ok', 'fixture_exit_code', 'fixture_copy_removed',
            'daemon_pid', 'daemon_start_utc_ticks', 'fixture_pid', 'fixture_start_utc_ticks', 'samples_written', 'max_seen_snapshots',
            'candidate_sha256', 'fixture_sha256')) {
        $bad = [ordered]@{}
        foreach ($entry in $context.Result.GetEnumerator()) { $bad[$entry.Key] = $entry.Value }
        $bad[$field] = $null
        Expect-SteadyReject { Assert-SteadyCompleted $bad } 'AB_STEADY_INCOMPLETE'
        $checks++
    }
    $failures = @{
        cancel = 'AB_STEADY_CANCELLED'; identity = 'AB_PROCESS_IDENTITY'; cli = 'AB_CLI_TIMEOUT'
        start_session = 'AB_CLI_TIMEOUT'; end_session = 'AB_CLI_TIMEOUT'
        replay = 'AB_SNAPSHOT_REUSED'; image = 'AB_IMAGE_PAYLOAD'; wrong_pid = 'AB_STATE_PID'
    }
    foreach ($failure in $failures.Keys) {
        $c = New-SteadyMemoryContext $failure
        Expect-SteadyReject { Invoke-SteadyObservations $c } $failures[$failure]
        Require-AB ($c.Result.status -ceq 'RUNNING' -and $c.Result.session_end_attempts -gt 0 -and
            $c.Result.session_end_attempts -le $c.Result.sessions_attempted) 'AB_SELFTEST_FAILURE_CLEANUP'
        $checks += 2
    }
    $c = New-SteadyMemoryContext '' 1
    Expect-SteadyReject { Invoke-SteadyObservations $c } 'AB_STEADY_SESSION_BUDGET'
    Require-AB ($c.Memory.Active.Count -eq 0 -and $c.Result.sessions_attempted -eq 1) 'AB_SELFTEST_BUDGET_CLEANUP'
    $checks += 2
    # 直接使用入口保存的默认参数，不另写一组会掩盖默认值漂移的慢 RPC 参数。
    $defaultPlan = New-SteadyPlan $steadyOptions
    Require-AB ($defaultPlan.duration_ms -eq 14400000 -and $defaultPlan.warmup -eq 25 -and
        $defaultPlan.session_observations -eq 200 -and $defaultPlan.interval_ms -eq 1000 -and
        $defaultPlan.max_sessions -eq 128) 'AB_SELFTEST_DEFAULT_PLAN'
    $checks++
    foreach ($rpcMs in @(0, 1)) {
        $c = New-SteadyMemoryContext -Plan $defaultPlan
        $c.Memory.CallAdvanceMs = $rpcMs
        Invoke-SteadyObservations $c
        Require-AB ($c.Result.steady_elapsed_ms -ge 14400000 -and $c.Result.observations_completed -gt 14000 -and
            $c.Result.warmup_completed -eq 25 -and $c.Result.sessions_started -eq 73 -and
            $c.Result.sessions_attempted -eq 73 -and $c.Result.sessions_ended -eq 73 -and
            $c.Memory.Active.Count -eq 0 -and $c.Seen.Count -eq 0 -and
            $c.Result.max_seen_snapshots -eq 200 -and $c.Result.post_end_samples -eq 3) 'AB_SELFTEST_FAST_RPC_FOUR_HOURS'
        $c.Result.daemon_cleanup_ok = $true; $c.Result.daemon_exit_code = -1
        $c.Result.fixture_cleanup_ok = $true; $c.Result.fixture_exit_code = 0
        $c.Result.fixture_copy_removed = $true
        Assert-SteadyCompleted $c.Result
        $checks += 2
        Write-Output ('STEADY_FAST_RPC_4H_PASS rpc_ms=' + $rpcMs + ' steady_ms=' + $c.Result.steady_elapsed_ms +
            ' observations=' + $c.Result.observations_completed + ' sessions=' + $c.Result.sessions_ended +
            '/' + $c.Plan.max_sessions + ' io=memory_only')
    }
    # 完整 4h 走相同调度循环，仅 RPC 与等待的耗时由虚拟钟推进；无真实睡眠或进程。
    $c = New-SteadyMemoryContext
    $c.Plan.duration_ms = 14400000L; $c.Plan.session_ms = 600000; $c.Plan.call_ms = 20000
    $c.Plan.session_observations = 200; $c.Plan.sample_ms = 30000; $c.Plan.max_sessions = 64
    $c.Memory.CallAdvanceMs = 10000
    Invoke-SteadyObservations $c
    Require-AB ($c.Result.steady_elapsed_ms -ge 14400000 -and $c.Result.sessions_started -gt 1 -and
        $c.Result.sessions_started -le 64 -and $c.Result.max_seen_snapshots -le 200 -and
        @($c.Memory.Samples | Where-Object { $_.metrics.driver.pid -ne 321 -or $_.metrics.fixture.pid -ne 123 }).Count -eq 0) 'AB_SELFTEST_FOUR_HOUR_LOOP'
    $checks++
    $c = New-SteadyMemoryContext
    $c.Plan.session_observations = 500
    $c.Plan.session_ms = 10000
    Invoke-SteadyObservations $c
    Require-AB ($c.Result.sessions_started -gt 1 -and $c.Result.max_seen_snapshots -lt 500) 'AB_SELFTEST_TIME_ROTATION'
    $checks++
    $c = New-SteadyMemoryContext
    $c.Memory.Now = $c.Plan.setup_ms
    Expect-SteadyReject { Invoke-SteadyObservations $c } 'AB_STEADY_DEADLINE'
    Require-AB ($c.Result.session_end_attempts -eq 1) 'AB_SELFTEST_DEADLINE_CLEANUP'
    $checks += 2
    $c = New-SteadyMemoryContext
    $c.Session = [pscustomobject]@{ StartMs = 0L }
    $c.Memory.Now = $c.Plan.session_ms - $c.Plan.call_ms
    Expect-SteadyReject { Get-SteadyDeadlineMs $c 'get_window_state' $false } 'AB_STEADY_DEADLINE'
    $checks++
    $c.CleanupStartMs = $c.Memory.Now - $c.Plan.cleanup_ms
    Expect-SteadyReject { Get-SteadyDeadlineMs $c 'end_session' $true } 'AB_STEADY_DEADLINE'
    $checks++
    $options = @{DurationSeconds=14400;Warmup=25;SessionSeconds=600;SessionObservationLimit=200;MaxSessions=64;IntervalMs=1000;CallMs=20000;SampleSeconds=30}
    $plan = New-SteadyPlan $options
    Require-AB ($plan.duration_ms -eq 14400000 -and $plan.manifest_seconds -eq 14940 -and
        ($plan.manifest_seconds + 60) * 1000 -lt 86400000) 'AB_SELFTEST_FOUR_HOUR_PLAN'
    $manifest = New-SteadyManifest 'C:\private\fixture.exe' $plan
    Assert-SteadyManifest $manifest 'C:\private\fixture.exe' $plan
    $checks += 2
    $manifest.allow.tools += 'click'
    Expect-SteadyReject { Assert-SteadyManifest $manifest 'C:\private\fixture.exe' $plan } 'AB_MANIFEST_TOOLS'
    $manifest = New-SteadyManifest 'C:\private\fixture.exe' $plan
    $manifest.expires_after = '24h'
    Expect-SteadyReject { Assert-SteadyManifest $manifest 'C:\private\fixture.exe' $plan } 'AB_STEADY_MANIFEST_LIFETIME'
    $manifest = New-SteadyManifest 'C:\private\fixture.exe' $plan
    $manifest.resources.desktop = @{display='all'}
    Expect-SteadyReject { Assert-SteadyManifest $manifest 'C:\private\fixture.exe' $plan } 'AB_MANIFEST_RESOURCE'
    $options.SessionSeconds = 60
    Expect-SteadyReject { New-SteadyPlan $options } 'AB_STEADY_SESSION_HEADROOM'
    $checks += 4
    Write-Output ('STEADY_SOURCE_SELFTEST_PASS checks=' + $checks + ' io=memory_only virtual_clock=true gui_driver_started=0')
}
