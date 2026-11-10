param(
    [int]$Seconds = 14400,
    [int]$IntervalMs = 5000,
    [string]$BundlePath = "",
    [ValidateRange(10,3600)][int]$DesktopTimeoutSeconds = 300
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'acceptance-report.ps1')
. (Join-Path $PSScriptRoot 'interactive-desktop.ps1')
if ([string]::IsNullOrWhiteSpace($BundlePath) -or -not (Test-Path -LiteralPath $BundlePath -PathType Container)) {
    throw '需要 -BundlePath 指向当前来源绑定的 Windows 验收包，再运行最终验收。'
}
$BundlePath = (Resolve-Path -LiteralPath $BundlePath).ProviderPath

$fixtureRoot = [IO.Path]::GetFullPath($PSScriptRoot)
$repoRoot = [IO.Path]::GetFullPath((Join-Path $fixtureRoot "..\..\.."))
$runId = [guid]::NewGuid().ToString('N')
$resultRoot = Join-Path $fixtureRoot ("results\final-run-" + $runId)
$null = New-Item -ItemType Directory -Path $resultRoot
$null = New-Item -ItemType Directory -Path (Join-Path $fixtureRoot 'target') -Force
$runLock = $null
$powerRequestHeld = $false
$statePath = Join-Path $fixtureRoot "target\final-run-state.json"
$e2eOutput = Join-Path $resultRoot "e2e.json"
$shortOutput = Join-Path $resultRoot "soak-60s.json"
$longOutput = Join-Path $resultRoot "soak-4h.json"

if (-not ("CortexFinalRunPower" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class CortexFinalRunPower {
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern uint SetThreadExecutionState(uint flags);
}
"@
}

function Write-RunState {
    param(
        [string]$Phase,
        [string]$Status,
        [string]$Failure = ""
    )
    [ordered]@{
        schemaVersion = 1
        phase = $Phase
        status = $Status
        updatedAt = [DateTime]::UtcNow.ToString("o")
        failure = $Failure
        runId = $runId
        resultDirectory = $resultRoot
    } | ConvertTo-Json -Depth 4 | ForEach-Object {
        # target 中只保留最新入口状态；本轮真相另存独立目录，不能覆盖旧 PASS。
        [IO.File]::WriteAllText((Join-Path $resultRoot 'state.json'), $_, [Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText($statePath, $_, [Text.UTF8Encoding]::new($false))
    }
}

function Test-InteractiveDesktop {
    return (Get-InteractiveDesktopEvidence).Allowed
}

function Wait-ForInteractiveDesktop {
    param(
        [int]$TimeoutSeconds = 300,
        [int]$StableMilliseconds = 10000,
        [scriptblock]$Read = { Test-InteractiveDesktop },
        [scriptblock]$Pause = { param($Milliseconds) Start-Sleep -Milliseconds $Milliseconds },
        [scriptblock]$Elapsed = $null
    )
    $clock = [Diagnostics.Stopwatch]::StartNew()
    if ($null -eq $Elapsed) { $Elapsed = { $clock.ElapsedMilliseconds }.GetNewClosure() }
    $stableSince = $null
    $deadline = [long]$TimeoutSeconds * 1000
    while ((& $Elapsed) -lt $deadline) {
        $allowed = & $Read
        $now = [long](& $Elapsed)
        if ($now -ge $deadline) { break }
        if ($allowed) {
            if ($null -eq $stableSince) {
                $stableSince = $now
            }
            if ($now - $stableSince -ge $StableMilliseconds) {
                return
            }
        }
        else {
            $stableSince = $null
        }
        Write-RunState -Phase "waiting_for_unlock" -Status "waiting"
        $left = $deadline - [long](& $Elapsed)
        if ($left -le 0) { break }
        & $Pause ([Math]::Min(3000, $left))
    }
    throw '等待交互桌面证据达到总预算，停止本轮验收；不能据此断言一定锁屏。'
}

function Assert-OwnedSoakReport {
    param($Report, [int]$RequiredSeconds)
    if ($RequiredSeconds -lt 60) { throw 'owned soak 的要求时长不能小于 60 秒。' }
    $elapsed = $Report.tests.soak.durationMilliseconds
    if ($elapsed -isnot [int] -and $elapsed -isnot [long]) { throw 'owned soak 时长必须是整数毫秒。' }
    if ($Report.status -cne 'passed' -or $Report.runtimeOwnership -cne 'agent' -or
        $Report.tests.secure.status -cne 'passed' -or $Report.tests.soak.status -cne 'passed' -or
        $Report.tests.secure.harness.passed -isnot [bool] -or -not $Report.tests.secure.harness.passed -or
        $Report.tests.soak.harness.passed -isnot [bool] -or -not $Report.tests.soak.harness.passed -or
        $elapsed -lt ([long]$RequiredSeconds * 1000)) {
        throw '真实 owned secure/soak 结果、精确单测试或完整时长未通过。'
    }
    foreach ($stage in @($Report.tests.secure, $Report.tests.soak)) {
        if ($stage.exitCode -ne 0 -or $stage.harness.runningOneLines -ne 1 -or
            $stage.harness.namedPassedLines -ne 1 -or $stage.harness.onePassedSummaryLines -ne 1) {
            throw 'owned fixture 必须恰好运行并通过指定测试一次。'
        }
    }
}

function Assert-OwnedSupervisorReport {
    param($Owner, [int]$RequiredSeconds, [string]$ResultPath)
    if ($Owner.suite -cne 'cortex-windows-portable-owned-soak' -or $Owner.status -cne 'passed' -or
        $Owner.exitCode -ne 0 -or $Owner.requestedSeconds -ne $RequiredSeconds -or
        $Owner.runtimeOwnership -cne 'agent' -or $Owner.runner.childExitCode -ne 0 -or
        $Owner.runner.timedOut -or $Owner.runner.activeAfterDrain -ne 0 -or
        $Owner.failures.Count -ne 0 -or $Owner.runner.cleanupErrors.Count -ne 0) {
        throw 'owned 监督器终态、请求时长或原 Job 结果未通过。'
    }
    foreach ($flag in @($Owner.ownerVerified, $Owner.sourceInputsUnchanged, $Owner.originalInputsUnchanged,
        $Owner.frozenInputsUnchanged, $Owner.toolSourcesUnchanged, $Owner.runner.originalJobDrained,
        $Owner.runner.powerRequestAcquired, $Owner.runner.powerRequestReleased)) {
        if ($flag -isnot [bool] -or -not $flag) { throw 'owned 监督器缺少严格的来源/排空/电源释放证据。' }
    }
    if ($Owner.runner.powerRequestActive -or
        [IO.Path]::GetFullPath($Owner.portableOutput) -ne [IO.Path]::GetFullPath($ResultPath)) {
        throw 'owned 监督结果未绑定本轮 portable 输出。'
    }
    $stream = [IO.File]::OpenRead($ResultPath)
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $actual = [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-','').ToLowerInvariant() }
    finally { $sha.Dispose(); $stream.Dispose() }
    if ($Owner.portableResultSha256 -cne $actual) { throw 'portable 结果在监督器完成后发生变化。' }
}

function Invoke-ChildPowerShell {
    param([string[]]$Arguments)

    # 证据写入 JSON；丢弃大体积 stdout，避免控制台排版延迟并污染数字退出码。
    & powershell.exe -NoProfile -ExecutionPolicy Bypass @Arguments | Out-Null
    return $LASTEXITCODE
}

# 与 soak 使用同一组标志。监视线程在整个 E2E 和长跑期间存活，退出时清除，
# 因而不持久修改系统电源策略。
$exitCode = 1
$failure = ""
try {
    $runLock = [IO.File]::Open((Join-Path $fixtureRoot 'target\final-run.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    if ([CortexFinalRunPower]::SetThreadExecutionState([uint32]2147483651) -eq 0) {
        throw 'Unable to hold the Windows display awake for the final run.'
    }
    $powerRequestHeld = $true
    Wait-ForInteractiveDesktop -TimeoutSeconds $DesktopTimeoutSeconds
    Write-RunState -Phase "windows_e2e" -Status "running"
    $exitCode = Invoke-ChildPowerShell -Arguments @(
        "-STA",
        "-File", (Join-Path $fixtureRoot "e2e.ps1"),
        "-NoBuild",
        "-OutputPath", $e2eOutput
    )
    if ($exitCode -ne 0) {
        throw "Windows E2E exited with code $exitCode."
    }
    $e2eResult = Read-E2EReport -Path $e2eOutput
    Assert-E2EReport -Report $e2eResult

    Write-RunState -Phase "soak_60s" -Status "running"
    $exitCode = Invoke-ChildPowerShell -Arguments @(
        "-File", (Join-Path $fixtureRoot "soak.ps1"),
        "-BundlePath", $BundlePath,
        "-Seconds", "60",
        "-IntervalMs", "$IntervalMs",
        "-OutputPath", $shortOutput
    )
    if ($exitCode -ne 0) {
        throw "60-second soak exited with code $exitCode."
    }
    Assert-OwnedSoakReport -Report (Read-E2EReport -Path $shortOutput) -RequiredSeconds 60
    Assert-OwnedSupervisorReport -Owner (Read-E2EReport -Path ($shortOutput + '.owner.json')) -RequiredSeconds 60 -ResultPath $shortOutput

    Write-RunState -Phase "soak_4h" -Status "running"
    $exitCode = Invoke-ChildPowerShell -Arguments @(
        "-File", (Join-Path $fixtureRoot "soak.ps1"),
        "-BundlePath", $BundlePath,
        "-Seconds", "$Seconds",
        "-IntervalMs", "$IntervalMs",
        "-OutputPath", $longOutput
    )
    if ($exitCode -ne 0) {
        throw "Long soak exited with code $exitCode."
    }
    Assert-OwnedSoakReport -Report (Read-E2EReport -Path $longOutput) -RequiredSeconds $Seconds
    Assert-OwnedSupervisorReport -Owner (Read-E2EReport -Path ($longOutput + '.owner.json')) -RequiredSeconds $Seconds -ResultPath $longOutput

    Write-RunState -Phase "complete" -Status "passed"
    $exitCode = 0
}
catch {
    $exitCode = 1
    $failure = $_.Exception.Message
    Write-RunState -Phase "failed" -Status "failed" -Failure $failure
}
finally {
    if ($powerRequestHeld) { $null = [CortexFinalRunPower]::SetThreadExecutionState([uint32]2147483648) }
    if ($null -ne $runLock) { $runLock.Dispose() }
}

if (-not [string]::IsNullOrWhiteSpace($failure)) {
    throw $failure
}
exit $exitCode
