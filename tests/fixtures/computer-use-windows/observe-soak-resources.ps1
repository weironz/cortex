param(
    [Parameter(Mandatory = $true)][string]$RunPath,
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [ValidateRange(1, 60)][int]$IntervalSeconds = 60,
    [ValidateRange(1, 14401)][int]$MaximumSamples = 241
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
foreach ($module in @('Microsoft.PowerShell.Utility', 'Microsoft.PowerShell.Management')) {
    Import-Module ([IO.Path]::Combine($PSHOME, 'Modules', $module, "$module.psd1"))
}
$run = Get-Content -LiteralPath $RunPath -Raw -Encoding UTF8 | ConvertFrom-Json
$roles = [ordered]@{ test = $run.testPid; driver = $run.driverPid; fixture = $run.fixturePid }
$identities = @{}
# PowerShell 7 可把 JSON 日期直接转成 DateTime；强制转字符串再 Parse 会丢精度。
$expectedTestStart = ([DateTimeOffset]$run.testStartedAt).UtcDateTime
foreach ($role in $roles.Keys) {
    $process = Get-Process -Id $roles[$role] -ErrorAction Stop
    try {
        $createdAt = $process.StartTime.ToUniversalTime()
        $nameMatches = switch ($role) {
            'test' { $process.ProcessName -match '^cortex_local-[0-9a-f]+$' }
            'driver' { $process.ProcessName -eq 'cua-driver' }
            'fixture' { $process.ProcessName -eq 'cortex-computer-use-soak-fixture' }
        }
        if (-not $nameMatches -or
            ($role -eq 'test' -and [Math]::Abs(($createdAt - $expectedTestStart).TotalMilliseconds) -gt 1) -or
            ($role -ne 'test' -and $createdAt -gt $expectedTestStart)) {
            throw "Process identity does not match the recorded soak: $role."
        }
        $identities[$role] = $createdAt.Ticks
    }
    finally { $process.Dispose() }
}
$output = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $output) { throw 'Observer output directory already exists.' }
$null = New-Item -ItemType Directory -Path $output
$samplesPath = Join-Path $output 'samples.jsonl'
$startedAt = [DateTime]::UtcNow
$reason = 'sample_limit'
$count = 0
$failure = $null
try {
    for ($sample = 0; $sample -lt $MaximumSamples; $sample++) {
        $metrics = @()
        $testEnded = $false
        foreach ($role in $roles.Keys) {
            $process = Get-Process -Id $roles[$role] -ErrorAction SilentlyContinue
            if ($null -eq $process) {
                $metrics += @{ role = $role; processId = $roles[$role]; state = 'exited' }
                if ($role -eq 'test') { $reason = 'test_exited'; $testEnded = $true }
                continue
            }
            try {
                # PID 可复用；绝不把后来恰好同 PID 的进程当成长跑进程。
                if ($process.StartTime.ToUniversalTime().Ticks -ne $identities[$role]) {
                    $metrics += @{ role = $role; processId = $roles[$role]; state = 'identity_changed' }
                    if ($role -eq 'test') { $reason = 'test_identity_changed'; $testEnded = $true }
                    continue
                }
                $metrics += [ordered]@{
                    role = $role
                    processId = $process.Id
                    state = 'running'
                    handles = $process.HandleCount
                    threads = $process.Threads.Count
                    privateBytes = $process.PrivateMemorySize64
                    workingSetBytes = $process.WorkingSet64
                    cpuSeconds = $process.TotalProcessorTime.TotalSeconds
                }
            }
            finally { $process.Dispose() }
        }
        $row = [ordered]@{ observedAt = [DateTime]::UtcNow.ToString('o'); processes = $metrics }
        $row | ConvertTo-Json -Depth 5 -Compress |
            Add-Content -LiteralPath $samplesPath -Encoding UTF8
        $count++
        if ($testEnded) { break }
        if ($sample + 1 -lt $MaximumSamples) { Start-Sleep -Seconds $IntervalSeconds }
    }
}
catch { $reason = 'observer_error'; $failure = $_.Exception.Message }
finally {
    [ordered]@{
        schemaVersion = 1
        scope = 'read-only process resource sampling; no desktop actions'
        startedAt = $startedAt.ToString('o')
        finishedAt = [DateTime]::UtcNow.ToString('o')
        samples = $count
        endReason = $reason
        failure = $failure
        note = 'Observation starts when this script runs, not when the soak started. These samples do not prove absence of leaks or a passing soak.'
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'summary.json') -Encoding UTF8
}
if ($null -ne $failure) { throw $failure }
