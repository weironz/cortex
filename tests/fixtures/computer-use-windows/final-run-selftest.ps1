param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# 只加载真实入口的函数，避免自测意外启动桌面任务或保持唤醒。
$runnerPath = Join-Path $PSScriptRoot 'final-run-on-unlock.ps1'
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    $runnerPath, [ref]$null, [ref]$parseErrors
)
if (@($parseErrors).Count -ne 0) { throw 'Final runner does not parse.' }
$functionAst = $ast.Find({
    param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Invoke-ChildPowerShell'
}, $true)
if ($null -eq $functionAst) { throw 'Child process runner was not found.' }
. ([scriptblock]::Create($functionAst.Extent.Text))

foreach ($expected in @(0, 7)) {
    $actual = @(Invoke-ChildPowerShell -Arguments @(
        '-Command', "Write-Output 'child stdout'; Write-Output '0'; exit $expected"
    ))
    if ($actual.Count -ne 1 -or $actual[0] -isnot [int] -or $actual[0] -ne $expected) {
        throw "Child stdout contaminated exit code $expected (items=$($actual.Count))."
    }
}
Write-Output 'PASS: noisy child stdout cannot change success or failure exit codes.'

foreach ($name in @('Wait-ForInteractiveDesktop','Assert-OwnedSoakReport','Assert-OwnedSupervisorReport')) {
    $function = $ast.Find({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name}, $true)
    if ($null -eq $function) { throw "Missing final-run contract function: $name" }
    . ([scriptblock]::Create($function.Extent.Text))
}
function Write-RunState { param($Phase,$Status) }
$script:desktopMs = 0
Wait-ForInteractiveDesktop -TimeoutSeconds 5 -StableMilliseconds 1000 -Read { $true } `
    -Elapsed { $script:desktopMs } -Pause {param($ms) $script:desktopMs += $ms}
if ($script:desktopMs -ne 3000) { throw 'Stable desktop must use the controlled evidence/time path.' }
$script:desktopMs = 0
$rejected = $false
try {
    Wait-ForInteractiveDesktop -TimeoutSeconds 2 -Read { $false } `
        -Elapsed { $script:desktopMs } -Pause {param($ms) $script:desktopMs += $ms}
} catch { $rejected = $true }
if (-not $rejected -or $script:desktopMs -ne 2000) { throw 'Missing desktop evidence must reach a bounded terminal failure.' }
$script:desktopMs = 0
$rejected = $false
try {
    Wait-ForInteractiveDesktop -TimeoutSeconds 2 -StableMilliseconds 0 -Read {$script:desktopMs=2001;$true} `
        -Elapsed { $script:desktopMs } -Pause {throw 'Expired query must not sleep.'}
} catch { $rejected = $true }
if (-not $rejected) { throw 'Evidence arriving after the deadline must not authorize the final run.' }
Write-Output 'PASS: desktop waiting is bounded and late evidence is rejected without real sleeps.'

$good = '{"status":"passed","runtimeOwnership":"agent","tests":{"secure":{"status":"passed","exitCode":0,"harness":{"passed":true,"runningOneLines":1,"namedPassedLines":1,"onePassedSummaryLines":1}},"soak":{"status":"passed","exitCode":0,"durationMilliseconds":60000,"harness":{"passed":true,"runningOneLines":1,"namedPassedLines":1,"onePassedSummaryLines":1}}}}'
Assert-OwnedSoakReport -Report ($good|ConvertFrom-Json) -RequiredSeconds 60
foreach ($bad in @(
    $good.Replace('"runtimeOwnership":"agent"','"runtimeOwnership":"external"'),
    $good.Replace('"durationMilliseconds":60000','"durationMilliseconds":59999'),
    $good.Replace('"durationMilliseconds":60000','"durationMilliseconds":"60000"'),
    $good.Replace('"runningOneLines":1','"runningOneLines":0'),
    $good.Replace('"status":"passed"','"status":"failed"')
)) {
    $rejected=$false
    try { Assert-OwnedSoakReport -Report ($bad|ConvertFrom-Json) -RequiredSeconds 60 } catch { $rejected=$true }
    if(-not $rejected){throw 'Invalid owned-mode, duration or exact-test evidence must fail the final run.'}
}
Write-Output 'PASS: owned soak report requires integer full duration and exact successful tests.'

$supervisedPath = Join-Path ([IO.Path]::GetTempPath()) ('cortex-supervised-result-' + [guid]::NewGuid().ToString('N') + '.json')
try {
    [IO.File]::WriteAllText($supervisedPath, $good, [Text.UTF8Encoding]::new($false))
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $resultHash = [BitConverter]::ToString($sha.ComputeHash([IO.File]::ReadAllBytes($supervisedPath))).Replace('-','').ToLowerInvariant() }
    finally { $sha.Dispose() }
    $owner = [ordered]@{suite='cortex-windows-portable-owned-soak';status='passed';exitCode=0;requestedSeconds=60;runtimeOwnership='agent';ownerVerified=$true;sourceInputsUnchanged=$true;originalInputsUnchanged=$true;frozenInputsUnchanged=$true;toolSourcesUnchanged=$true;failures=@();portableOutput=$supervisedPath;portableResultSha256=$resultHash;runner=@{childExitCode=0;timedOut=$false;activeAfterDrain=0;cleanupErrors=@();originalJobDrained=$true;powerRequestAcquired=$true;powerRequestReleased=$true;powerRequestActive=$false}}
    $ownerJson=$owner|ConvertTo-Json -Depth 6
    Assert-OwnedSupervisorReport -Owner ($ownerJson|ConvertFrom-Json) -RequiredSeconds 60 -ResultPath $supervisedPath
    foreach($field in @('originalJobDrained','powerRequestReleased','sourceInputsUnchanged','frozenInputsUnchanged')) {
        $bad=$ownerJson|ConvertFrom-Json
        if($field -in @('originalJobDrained','powerRequestReleased')){$bad.runner.$field=$false}else{$bad.$field=$false}
        $rejected=$false
        try{Assert-OwnedSupervisorReport -Owner $bad -RequiredSeconds 60 -ResultPath $supervisedPath}catch{$rejected=$true}
        if(-not $rejected){throw "Missing supervision evidence was accepted: $field"}
    }
    [IO.File]::AppendAllText($supervisedPath, ' ')
    $rejected=$false
    try{Assert-OwnedSupervisorReport -Owner ($ownerJson|ConvertFrom-Json) -RequiredSeconds 60 -ResultPath $supervisedPath}catch{$rejected=$true}
    if(-not $rejected){throw 'Mutated portable output was accepted.'}
} finally { if([IO.File]::Exists($supervisedPath)){[IO.File]::Delete($supervisedPath)} }
Write-Output 'PASS: supervisor result requires source integrity, original Job drain, power release and result hash binding.'

. (Join-Path $PSScriptRoot 'acceptance-report.ps1')
$unicodePath = Join-Path ([IO.Path]::GetTempPath()) ('cortex-report-' + [guid]::NewGuid().ToString('N') + '.json')
try {
    [IO.File]::WriteAllText($unicodePath, '{"label":"无标题。未修改。"}', [Text.UTF8Encoding]::new($false))
    $unicode = Read-E2EReport $unicodePath
    if ($unicode.label -cne '无标题。未修改。') { throw 'BOM-less UTF-8 report was corrupted.' }
}
finally { [IO.File]::Delete($unicodePath) }
Write-Output 'PASS: BOM-less Chinese UTF-8 reports parse correctly on Windows PowerShell.'
$report = [pscustomobject]@{
    suite = 'cortex-computer-use-windows-e2e'
    host = [pscustomobject]@{ desktopLockedAtReport = $false }
    summary = [pscustomobject]@{ total = 20; pass = 20; fail = 0; skipped = 0 }
    tasks = @(1..20 | ForEach-Object {
        [pscustomobject]@{ id = "task-$_"; status = 'pass'; reason = $null }
    })
}
Assert-E2EReport $report
foreach ($scenario in @('failed-task', 'locked', 'incomplete', 'unexpected-skip')) {
    $bad = $report | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    switch ($scenario) {
        'failed-task' { $bad.tasks[0].status = 'fail'; $bad.summary.pass = 19; $bad.summary.fail = 1 }
        'locked' { $bad.host.desktopLockedAtReport = $true }
        'incomplete' { $bad.tasks = @($bad.tasks | Select-Object -First 19) }
        'unexpected-skip' {
            $bad.tasks[0].status = 'skipped'; $bad.tasks[0].reason = 'test'
            $bad.summary.pass = 19; $bad.summary.skipped = 1
        }
    }
    $rejected = $false
    try { Assert-E2EReport $bad } catch { $rejected = $true }
    if (-not $rejected) { throw "Invalid report was accepted: $scenario" }
}
Write-Output 'PASS: failed, locked, partial and unexpected-skip reports are rejected.'

# 源码目录故意不带 bundle 清单；验证失败也必须写本轮独立状态，不能复用旧 PASS。
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
$relativeResults = 'target/second-host-wrapper-selftest-' + [guid]::NewGuid().ToString('N')
$resultPaths = @()
Push-Location $repoRoot
try {
    foreach ($attempt in @(1, 2)) {
        $previousErrorAction = $ErrorActionPreference
        try {
            $ErrorActionPreference = 'Continue'
            & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (
                Join-Path $PSScriptRoot 'run-second-host-acceptance.ps1'
            ) -ResultDirectory $relativeResults 2>$null
            $wrapperExitCode = $LASTEXITCODE
        }
        finally { $ErrorActionPreference = $previousErrorAction }
        if ($wrapperExitCode -eq 0) { throw 'Wrapper accepted a directory without a bundle manifest.' }
        $latest = Get-Content (Join-Path $relativeResults 'latest-run.json') -Raw | ConvertFrom-Json
        if (-not [IO.Path]::IsPathRooted($latest.summaryPath)) {
            throw 'Wrapper did not normalize a relative result path.'
        }
        $summary = Get-Content -LiteralPath $latest.summaryPath -Raw | ConvertFrom-Json
        if ($summary.result -ne 'failed') { throw 'Wrapper retained a stale passing summary.' }
        $resultPaths += $latest.summaryPath
    }
    if ($resultPaths[0] -eq $resultPaths[1]) { throw 'Wrapper reused a prior run directory.' }
}
finally { Pop-Location }
Write-Output 'PASS: relative result roots support repeat runs and persist each failure.'
