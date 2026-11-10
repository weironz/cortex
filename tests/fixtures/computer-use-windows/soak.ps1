<#
.SYNOPSIS
    从当前构建验收包运行 portable/owned soak，不编译、不外启 Cua daemon。
.DESCRIPTION
    仅支持系统 64 位 Windows PowerShell 5.1。bundle 必须含 SHA256SUMS.json、
    test-artifact-proof.json、cargo-messages.ndjson 及当前 portable runner 的输入。
    sourceProof 只是本机构建关联记录，不是签名、第二台执行或生产验收。

    OutputPath 保持 portable runner 原样生成的 JSON；监督证据另写
    <OutputPath>.owner.json。调用方必须同时检查本进程退出码和 owner.status，
    因为 portable 测试成功后仍可能在来源变化、Job 排空或电源释放门禁失败。
    OutputDirectory 是可选替代，必须是全新目录，里面生成 portable.json 与
    portable.json.owner.json。不提供输出参数时使用全新系统临时目录。

    ValidateOnly 不创建结果/目录，不启动任何进程，也不证明 owner、测试或桌面通过。
    实际运行保留冻结副本、来源摘要、runner 日志、nonce/PID/心跳及终态证据。
.EXAMPLE
    C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe -NoProfile -File .\soak.ps1 `
      -BundlePath 'D:\transfer\current-bundle' -Seconds 14400 -IntervalMs 5000 `
      -OutputPath 'D:\results\fresh-soak.json' -ValidateOnly
#>
[CmdletBinding()]
param(
    [string]$BundlePath = '',
    [ValidateRange(60, 21600)][int]$Seconds = 14400,
    [ValidateRange(100, 60000)][int]$IntervalMs = 5000,
    [string]$OutputPath = '',
    [string]$OutputDirectory = '',
    [switch]$ValidateOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'soak-support.ps1')
$shell = Assert-CortexSoakShell
if ([string]::IsNullOrWhiteSpace($BundlePath)) {
    throw 'SOAK_BUNDLE_REQUIRED：请提供 -BundlePath <当前构建验收包目录，内含 SHA256SUMS.json 和 test-artifact-proof.json>；例如 soak.ps1 -BundlePath D:\transfer\current-bundle -Seconds 60 -OutputPath D:\results\fresh.json -ValidateOnly。不会猜测旧 exe/driver，也不会运行 Cargo。'
}
if ($OutputPath -and $OutputDirectory) { throw 'SOAK_OUTPUT_EXCLUSIVE：OutputPath 与 OutputDirectory 只能选一个。' }
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
$nonce = [guid]::NewGuid().ToString('N')
$startedAt = [DateTime]::UtcNow.ToString('o')
$portable = Join-Path $PSScriptRoot 'portable-second-host-smoke.ps1'
$jobHelper = Join-Path $PSScriptRoot 'readonly-ab\owned-job.cs'
$locks = [Collections.Generic.List[IO.FileStream]]::new()
$ownerStream = $null; $heartbeatStream = $null; $record = $null
$code = 1

try {
    $toolRows = @(foreach ($path in @($PSCommandPath, (Join-Path $PSScriptRoot 'soak-support.ps1'),
        (Join-Path $PSScriptRoot 'test-artifact-provenance.ps1'), $jobHelper, $portable)) {
        [ordered]@{ relativePath=[IO.Path]::GetFileName($path); path=$path;
            sha256=(Open-CortexSoakInput $path '' $locks) }
    })
    $bundle = Get-CortexSoakBundle $BundlePath $repositoryRoot $portable $locks
    if ($OutputPath) {
        $OutputPath = Resolve-CortexSoakPath $OutputPath
        if ([IO.Path]::GetExtension($OutputPath) -ine '.json') { throw 'SOAK_JSON_OUTPUT_REQUIRED' }
        $runRoot = $OutputPath + '.run-' + $nonce
    }
    else {
        if (-not $OutputDirectory) { $OutputDirectory = Join-Path ([IO.Path]::GetTempPath()) ('cortex-owned-soak-' + $nonce) }
        $runRoot = Resolve-CortexSoakPath $OutputDirectory
        $OutputPath = Join-Path $runRoot 'portable.json'
    }
    $ownerPath = $OutputPath + '.owner.json'
    foreach ($path in @($runRoot, $OutputPath, $ownerPath)) {
        $null = Resolve-CortexSoakPath $path
        if (Test-Path -LiteralPath $path) { throw "SOAK_OUTPUT_EXISTS：$path；请提供全新输出，不覆盖旧结果。" }
        foreach ($inputRoot in @($bundle.Root) + @($bundle.Proof.context.sourceRoots) +
            @((Join-Path $repositoryRoot 'migrations'), (Join-Path $repositoryRoot 'migrations-global'), (Join-Path $PSScriptRoot 'src'))) {
            if ($path -ieq $inputRoot -or $path.StartsWith($inputRoot.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
                throw 'SOAK_OUTPUT_OVERLAPS_INPUT：输出不得写入 bundle、源码包或测试脚本目录。'
            }
        }
    }
    if (-not [IO.Directory]::Exists([IO.Path]::GetDirectoryName($runRoot)) -or
        ($OutputDirectory -eq '' -and -not [IO.Directory]::Exists([IO.Path]::GetDirectoryName($OutputPath)))) {
        throw 'SOAK_OUTPUT_PARENT_MISSING：请先准备输出父目录。'
    }
    # 在子作用域复用当前 portable 的 pin/BOM/语法/单测试解析检查；这条路只读且不启动进程。
    $validationOutput = Join-Path ([IO.Path]::GetDirectoryName($runRoot)) ('soak-validation-' + $nonce + '.json')
    $validationArguments = @(Get-CortexSoakPortableArguments $bundle.Root $bundle.Manifest $validationOutput $Seconds $IntervalMs)
    # PS 脚本调用必须用命名 splat；字符串 argv 只用于后面的原生 powershell.exe。
    $validationParameters = @{}
    for ($index=0; $index -lt $validationArguments.Count; $index+=2) {
        $validationParameters[$validationArguments[$index].Substring(1)] = $validationArguments[$index+1]
    }
    & $portable @validationParameters -ValidateOnly | Out-Null
    if ($ValidateOnly) {
        [ordered]@{ status='validated_only'; processesStarted=0; ownerVerified=$false; runtimeOwnership=$null;
            bundlePath=$bundle.Root; portableOutput=$OutputPath; ownerOutput=$ownerPath;
            sourceInputsSha256=$bundle.Sources.sha256; sourceProofScope='local_build_source_association_only';
            signedProof=$false; secondHostExecuted=$false; productionAccepted=$false } | ConvertTo-Json
        exit 0
    }

    New-CortexSoakPrivateDirectory $runRoot
    $ownerStream = [IO.File]::Open($ownerPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $runner = [ordered]@{ childPid=$null; childExitCode=$null; startedAt=$null; finishedAt=$null;
        elapsedMilliseconds=0L; timedOut=$false; originalJobDrained=$false; activeBeforeDrain=$null; activeAfterDrain=$null;
        powerRequestAcquired=$false; powerRequestActive=$false; powerRequestReleased=$false;
        cleanupErrors=[Collections.Generic.List[string]]::new() }
    $record = [ordered]@{
        schemaVersion=1; suite='cortex-windows-portable-owned-soak'; status='failed'; nonce=$nonce;
        wrapperPid=$PID; startedAt=$startedAt; finishedAt=$null; exitCode=1; powershellVersion=$PSVersionTable.PSVersion.ToString();
        requestedSeconds=$Seconds; intervalMs=$IntervalMs; expectedRuntimeOwnership='agent'; runtimeOwnership=$null;
        ownerVerified=$false; portableOutput=$OutputPath; portableResultSha256=$null; runDirectory=$runRoot;
        scope='real_http_engine_localhost_private_authority_native_owned_fixture_with_fake_model_remote_approval';
        sourceProof=[ordered]@{ scope='local_build_source_association_only'; signed=$false;
            testExeSha256=$bundle.Proof.testExeSha256; inputsSha256=$bundle.Sources.sha256;
            proofSha256=$bundle.Manifest.sha256.testArtifactProof; ndjsonSha256=$bundle.Proof.ndjsonSha256 };
        secondHostExecuted=$false; productionAccepted=$false; humanApproval=$false; realUserAccount=$false;
        sourceFingerprintBefore=$bundle.Sources.sha256; sourceFingerprintAfter=$null; sourceInputsUnchanged=$false;
        originalInputsUnchanged=$false; frozenInputsUnchanged=$false; toolSourcesUnchanged=$false;
        toolSources=$toolRows; runner=$runner; failures=[Collections.Generic.List[string]]::new()
    }
    $frozenRows = [Collections.Generic.List[object]]::new()
    try {
        $frozen = Join-Path $runRoot 'bundle'; $control = Join-Path $runRoot 'control'
        foreach ($directory in @($frozen, $control, (Join-Path $frozen 'cua'))) {
            $null = [IO.Directory]::CreateDirectory($directory)
        }
        foreach ($pair in @(@{Rows=$bundle.Rows;Root=$frozen}, @{Rows=$toolRows;Root=$control})) {
            foreach ($row in $pair.Rows) {
                $copy = Join-Path $pair.Root $row.relativePath
                [IO.File]::Copy($row.path, $copy, $false)
                $hash = Open-CortexSoakInput $copy $row.sha256 $locks
                $frozenRows.Add([ordered]@{ relativePath=$row.relativePath; path=$copy; sha256=$hash })
            }
        }
        $snapshotPath = Join-Path $runRoot 'sources-before.json'
        $snapshot = [IO.File]::Open($snapshotPath, 'CreateNew', 'Write', 'Read')
        try { Write-CortexSoakJson $snapshot $bundle.Sources } finally { $snapshot.Dispose() }
        $inputs = [IO.File]::Open((Join-Path $runRoot 'inputs.json'), 'CreateNew', 'Write', 'Read')
        try { Write-CortexSoakJson $inputs ([ordered]@{ original=$bundle.Rows; frozen=$frozenRows.ToArray() }) }
        finally { $inputs.Dispose() }
        $runtimeRoot = Join-Path ([IO.Path]::GetTempPath()) ('cortex-soak-runtime-' + $nonce)
        if ([IO.Path]::GetPathRoot($runtimeRoot) -ine 'C:\') { throw 'SOAK_C_DRIVE_TEMP_REQUIRED：当前 portable 私有 authority 要求 C 盘临时目录。' }
        New-CortexSoakPrivateDirectory $runtimeRoot
        $record['runtimeDirectory'] = $runtimeRoot
        $environment = New-CortexSoakEnvironment $runtimeRoot $shell $nonce
        $arguments = @('-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File',
            (Join-Path $frozen 'portable-second-host-smoke.ps1')) +
            @(Get-CortexSoakPortableArguments $frozen $bundle.Manifest $OutputPath $Seconds $IntervalMs)
        $heartbeatPath = Join-Path $runRoot 'heartbeat.json'
        $heartbeatStream = [IO.File]::Open($heartbeatPath, 'CreateNew', 'Write', 'Read')
        $record['heartbeatPath'] = $heartbeatPath
        $record['runnerLog'] = Join-Path $runRoot 'runner.log'
        $audit = @{ launchWritten=$false; sequence=0L }
        $heartbeat = {
            param($state)
            $audit.sequence++
            $event = [ordered]@{ nonce=$nonce; wrapperPid=$PID; runnerPid=$state.childPid;
                sequence=$audit.sequence; observedAt=[DateTime]::UtcNow.ToString('o');
                elapsedMilliseconds=$state.elapsedMilliseconds; phase='running'; ownerVerified=$false }
            if (-not $audit.launchWritten) {
                $launch = [IO.File]::Open((Join-Path $runRoot 'launch.json'), 'CreateNew', 'Write', 'Read')
                try { Write-CortexSoakJson $launch $event } finally { $launch.Dispose() }
                $audit.launchWritten=$true
            }
            Write-CortexSoakJson $heartbeatStream $event
        }
        Write-Output "SOAK_RUNNING nonce=$nonce portable=$OutputPath owner=$ownerPath"
        Invoke-CortexSoakOwnedRunner $shell ([string[]]$arguments) $runRoot $record.runnerLog $environment `
            (Join-Path $control 'owned-job.cs') (($Seconds + 600) * 1000) $runner $heartbeat
        if ($runner.childExitCode -ne 0) { throw "SOAK_RUNNER_EXIT：$($runner.childExitCode)" }
    }
    catch { $record.failures.Add($_.Exception.Message) }
    finally {
        # 此时 runner 原 Job 已经清退；检查所有原输入、冻结副本和本机源码，绝不拼接旧 PASS。
        foreach ($check in @(@{Field='originalInputsUnchanged';Rows=$bundle.Rows},
            @{Field='frozenInputsUnchanged';Rows=$frozenRows.ToArray()}, @{Field='toolSourcesUnchanged';Rows=$toolRows})) {
            try { Assert-CortexSoakUnchanged $check.Rows; $record[$check.Field]=$true }
            catch { $record.failures.Add($_.Exception.Message) }
        }
        try {
            $after = Get-CortexSoakSources $bundle.Proof.context $repositoryRoot
            $record.sourceFingerprintAfter = $after.sha256
            $record.sourceInputsUnchanged = $after.sha256 -ceq $record.sourceFingerprintBefore
            $afterStream = [IO.File]::Open((Join-Path $runRoot 'sources-after.json'), 'CreateNew', 'Write', 'Read')
            try { Write-CortexSoakJson $afterStream $after } finally { $afterStream.Dispose() }
            if (-not $record.sourceInputsUnchanged) { throw 'SOAK_SOURCE_CHANGED' }
        }
        catch { $record.failures.Add($_.Exception.Message) }
        try {
            if ([IO.File]::Exists($OutputPath)) {
                $record.portableResultSha256 = Get-CortexArtifactSha256 $OutputPath
                $result = Read-CortexSoakJson $OutputPath
                Assert-CortexSoakPortableResult $result $bundle $Seconds $IntervalMs $runner
            }
            else { throw 'SOAK_PORTABLE_RESULT_MISSING' }
            if ($record.failures.Count -eq 0) {
                $record.ownerVerified=$true; $record.runtimeOwnership='agent'; $record.status='passed'; $code=0
            }
        }
        catch { $record.failures.Add($_.Exception.Message) }
        if ($record.status -cne 'passed') {
            $code = if ($runner.timedOut) { 124 } elseif ($null -ne $runner.childExitCode -and $runner.childExitCode -ne 0) {
                [int]$runner.childExitCode
            } else { 1 }
        }
        $record.exitCode=$code; $record.finishedAt=[DateTime]::UtcNow.ToString('o')
        if ($null -ne $heartbeatStream) {
            Write-CortexSoakJson $heartbeatStream ([ordered]@{ nonce=$nonce; wrapperPid=$PID; runnerPid=$runner.childPid;
                observedAt=$record.finishedAt; phase='terminal'; status=$record.status; exitCode=$code;
                originalJobDrained=$runner.originalJobDrained; ownerVerified=$record.ownerVerified })
        }
        Write-CortexSoakJson $ownerStream $record
    }
}
finally {
    if ($null -ne $heartbeatStream) { $heartbeatStream.Dispose() }
    if ($null -ne $ownerStream) { $ownerStream.Dispose() }
    foreach ($stream in $locks) { $stream.Dispose() }
}
Write-Output "SOAK_EXIT code=$code portable=$OutputPath owner=$ownerPath"
exit $code
