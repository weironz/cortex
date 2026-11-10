<#
.SYNOPSIS
    有限 headless 回归：只用假 bundle/源码/命令和原 Job 受控的 PowerShell child。
.DESCRIPTION
    不运行 GUI、driver、Cargo 或真实 soak。所有结果只证明监督与校验逻辑。
    保留全新临时目录供审计，不删除任何用户目录。
.EXAMPLE
    C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe -NoProfile -File .\soak-selftest.ps1
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'soak-support.ps1')
$shell = Assert-CortexSoakShell
$fixtureRoot = $PSScriptRoot
$root = Join-Path ([IO.Path]::GetTempPath()) ('cortex-soak-selftest-' + [guid]::NewGuid().ToString('N'))
New-CortexSoakPrivateDirectory $root
$checks = 0; $caseNumber = 0; $invocations = 0
$previousCargoHome = $env:CARGO_HOME
$jobHelper = Join-Path $fixtureRoot 'readonly-ab\owned-job.cs'
if (-not ('CortexReadonlyOwned.OwnedJob' -as [type])) { Add-Type -Path $jobHelper }

function Require-SoakSelfTest {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw "SOAK_SELFTEST：$Message" }
    $script:checks++
}
function Expect-SoakReject {
    param([scriptblock]$Action, [string]$Tag)
    $rejected = $false
    try { $null = & $Action }
    catch { $rejected = -not $Tag -or $_.Exception.Message.Contains($Tag) }
    Require-SoakSelfTest $rejected ("应拒绝：" + $Tag)
}
function Write-SoakFakeFile {
    param([string]$Path, [string]$Text, [bool]$Bom = $false)
    $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path))
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($Bom))
}
function Write-SoakFakeJson {
    param([string]$Path, $Value)
    Write-SoakFakeFile $Path (ConvertTo-Json -InputObject $Value -Depth 40 -Compress)
}
function New-SoakFakeResult {
    param($Manifest)
    $tests = [ordered]@{}
    foreach ($phase in @('secure', 'soak')) {
        $name = if ($phase -ceq 'secure') { 'computer::real::tests::cua_secure_fixture_requires_handoff_without_input' }
            else { 'computer::real::tests::cua_fixture_long_running_soak' }
        $tests[$phase] = [ordered]@{ name=$name; status='passed'; exitCode=0; durationMilliseconds=60000L;
            harness=[ordered]@{ exitCode=0; runningOneLines=1; namedPassedLines=1; onePassedSummaryLines=1;
                totalRunLines=1; totalSummaryLines=1; passed=$true } }
    }
    $inputs = [ordered]@{ cua=[ordered]@{} }
    foreach ($binding in @(@('test','testExe'), @('fixture','fixtureExe'), @('vcRuntime','vcRuntime'))) {
        $hash = $Manifest.sha256.($binding[1])
        $inputs[$binding[0]] = @{ sha256=$hash; expectedSha256=$hash; executionSha256=$hash }
    }
    foreach ($name in $Manifest.sha256.cua.Keys) {
        $hash=$Manifest.sha256.cua[$name]; $inputs.cua[$name]=@{sha256=$hash;executionSha256=$hash}
    }
    return [ordered]@{ schemaVersion=1; suite='cortex-computer-use-windows-portable-second-host-smoke';
        status='passed'; runtimeOwnership='agent'; requestedSeconds=60; intervalMs=5000; tests=$tests; inputs=$inputs }
}
function New-SoakFakeCase {
    param([string]$Mode = 'exit7')
    $script:caseNumber++
    $caseRoot = Join-Path $root ('case-' + $script:caseNumber)
    $workspace = Join-Path $caseRoot '源码 space'
    $fixture = Join-Path $workspace 'tests\fixtures\computer-use-windows'
    $bundlePath = Join-Path $caseRoot 'bundle'
    $cargoHomePath = Join-Path $caseRoot 'fake-cargo-home'
    $env:CARGO_HOME = $cargoHomePath
    $null = [IO.Directory]::CreateDirectory($fixture)
    foreach ($name in @('soak.ps1', 'soak-support.ps1', 'test-artifact-provenance.ps1', 'readonly-ab\owned-job.cs')) {
        $destination=Join-Path $fixture $name
        $null=[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination))
        [IO.File]::Copy((Join-Path $fixtureRoot $name), $destination, $false)
    }
    Write-SoakFakeFile (Join-Path $workspace 'Cargo.toml') ('[workspace]' + "`n" + 'members = ["crates/cortex-local"]' + "`n[workspace.package]`n" + 'version = "9.8.7"' + "`n")
    Write-SoakFakeFile (Join-Path $workspace 'Cargo.lock') '假锁文件'
    $manifestPath = Join-Path $workspace 'crates\cortex-local\Cargo.toml'
    $source = Join-Path $workspace 'crates\cortex-local\src\main.rs'
    Write-SoakFakeFile $manifestPath '[package]'
    Write-SoakFakeFile $source '// 假源码，绝不构建'
    $context = [ordered]@{ repositoryRoot=$workspace; version='9.8.7'; packageId='path+file:///fake/cortex-local#9.8.7';
        manifestPath=$manifestPath; sourcePath=$source; sourceRoots=@([IO.Path]::GetDirectoryName($manifestPath));
        targetDirectory=(Join-Path $caseRoot 'fake-target') }
    $sources = Get-CortexArtifactInputs $context
    $exe = Join-Path $context.targetDirectory 'x86_64-pc-windows-msvc\release\deps\cortex_local-fake.exe'
    $artifact = [ordered]@{ reason='compiler-artifact'; package_id=$context.packageId; manifest_path=$manifestPath;
        target=[ordered]@{name='cortex-local';kind=@('bin');src_path=$source}; profile=@{test=$true};
        executable=$exe; filenames=@($exe); fresh=$false }
    Write-SoakFakeFile (Join-Path $bundlePath 'cargo-messages.ndjson') ((ConvertTo-Json -InputObject $artifact -Depth 20 -Compress) + "`n" + '{"reason":"build-finished","success":true}')
    $hashes = [ordered]@{ cua=[ordered]@{} }
    foreach ($entry in @(@('testExe','cortex-local-tests.exe'), @('fixtureExe','cortex-computer-use-soak-fixture.exe'), @('vcRuntime','vcruntime140.dll'))) {
        $path = Join-Path $bundlePath $entry[1]
        Write-SoakFakeFile $path ('假文件，严禁执行：' + $entry[1]); $hashes[$entry[0]]=Get-CortexArtifactSha256 $path
    }
    foreach ($name in @('cua-driver.exe', 'cua-driver-uia.exe', 'cua-cursor-theme.exe', 'cua_driver_sdk.dll',
        'cua_driver_node_runtime.node', 'cua_driver_abi.h', 'LICENSE.md', 'THIRD_PARTY_NOTICES.md', 'SBOM.spdx.json', 'README.md')) {
        $path=Join-Path $bundlePath ('cua\'+$name); Write-SoakFakeFile $path ('假 Cua：'+$name)
        $hashes.cua[$name]=Get-CortexArtifactSha256 $path
    }
    $manifest = [ordered]@{schemaVersion=1;version='9.8.7';sha256=$hashes;
        testArtifact=@{target='x86_64-pc-windows-msvc';profile='release';sourceInputsSha256=$sources.sha256;cargoFresh=$false}}
    $fakeResult = New-SoakFakeResult $manifest
    # 替身只创建无 GUI 的睡眠孙进程；真监督器必须清退原 Job，不能靠测试 harness 代清理。
    $fakeRunner = @'
param([string]$TestExePath,[string]$FixtureExePath,[string]$VCRuntimePath,[string]$CuaDirectory,
    [string]$ExpectedTestSha256,[string]$ExpectedFixtureSha256,[string]$ExpectedVCRuntimeSha256,
    [string]$OutputPath,[int]$Seconds,[int]$IntervalMs,[switch]$ValidateOnly)
if ($ValidateOnly) { return }
$ErrorActionPreference='Stop'
$start=[Diagnostics.ProcessStartInfo]::new()
$start.FileName=[Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
$start.Arguments='-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 120"'
$start.UseShellExecute=$false; $start.CreateNoWindow=$true
$descendant=[Diagnostics.Process]::Start($start)
$frozenWriteBlocked=$false
try { [IO.File]::AppendAllText($TestExePath,'意外写入') } catch { $frozenWriteBlocked=$true }
$probe=@{grandchildPid=$descendant.Id;path=$env:PATH;owner=[Environment]::GetEnvironmentVariable('CORTEX_CUA_RUNTIME_OWNER');
    frozenWriteBlocked=$frozenWriteBlocked;nonce=$env:CORTEX_SOAK_RUN_NONCE}
[IO.File]::WriteAllText(($OutputPath+'.probe.json'),($probe|ConvertTo-Json))
if ('__MODE__' -ceq 'source-change') { [IO.File]::AppendAllText('__SOURCE__','变化') }
[IO.File]::WriteAllText($OutputPath,'__RESULT__',[Text.UTF8Encoding]::new($false))
if ('__MODE__' -ceq 'exit7') { exit 7 }
exit 0
'@
    $fakeRunner=$fakeRunner.Replace('__MODE__',$Mode).Replace('__SOURCE__',$source.Replace("'","''")).Replace('__RESULT__',
        (ConvertTo-Json -InputObject $fakeResult -Depth 20 -Compress).Replace("'","''"))
    Write-SoakFakeFile (Join-Path $fixture 'portable-second-host-smoke.ps1') $fakeRunner $true
    Write-SoakFakeFile (Join-Path $bundlePath 'portable-second-host-smoke.ps1') $fakeRunner $true
    $hashes['portableRunner']=Get-CortexArtifactSha256 (Join-Path $bundlePath 'portable-second-host-smoke.ps1')
    $proof = [ordered]@{schemaVersion=1;purpose='local-cargo-test-artifact-provenance';target='x86_64-pc-windows-msvc';
        profile='release';cargoExitCode=0;cargoArguments=@(Get-CortexArtifactBuildArguments);context=$context;
        inputsBefore=$sources;inputsAfter=$sources;buildEnvironmentBefore=('a'*64);buildEnvironmentAfter=('a'*64);
        artifact=$artifact;testExeSha256=$hashes.testExe;ndjsonSha256=(Get-CortexArtifactSha256 (Join-Path $bundlePath 'cargo-messages.ndjson'))}
    Write-SoakFakeJson (Join-Path $bundlePath 'test-artifact-proof.json') $proof
    $hashes['testArtifactProof']=Get-CortexArtifactSha256 (Join-Path $bundlePath 'test-artifact-proof.json')
    $hashes['testArtifactNdjson']=$proof.ndjsonSha256
    Write-SoakFakeJson (Join-Path $bundlePath 'SHA256SUMS.json') $manifest
    return [pscustomobject]@{Root=$caseRoot;OutputRoot=$caseRoot;Workspace=$workspace;Fixture=$fixture;Bundle=$bundlePath;Manifest=$manifest;
        Proof=$proof;Result=$fakeResult;CargoHome=$cargoHomePath;Source=$source}
}
function Invoke-SoakFakeEntry {
    param($Case, [string[]]$Extra = @(), [bool]$NoBundle = $false, [switch]$ExistingOutput, [switch]$DirectoryOutput)
    $script:invocations++
    $output=Join-Path $Case.OutputRoot ('result-'+$script:invocations+'.json')
    $outputDirectory=Join-Path $Case.Root ('fresh-output-'+$script:invocations)
    if ($DirectoryOutput) { $output=Join-Path $outputDirectory 'portable.json' }
    if ($ExistingOutput) { Write-SoakFakeFile $output '保留用户已有结果' }
    $log=Join-Path $Case.Root ('entry-'+$script:invocations+'.log')
    $launcher=Join-Path $Case.Root ('launcher-'+$script:invocations)
    $null=[IO.Directory]::CreateDirectory($launcher)
    $variables=New-CortexSoakEnvironment $launcher $shell 'headless-launcher'
    $variables['CARGO_HOME']=$Case.CargoHome
    $variables['PATH']+=';C:\fake-path-must-not-be-inherited'
    $variables['CORTEX_CUA_RUNTIME_OWNER']='external'
    $arguments=@('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',(Join-Path $Case.Fixture 'soak.ps1'), '-Seconds','60')
    if ($DirectoryOutput) { $arguments+=@('-OutputDirectory',$outputDirectory) }
    else { $arguments+=@('-OutputPath',$output) }
    if (-not $NoBundle) { $arguments+=@('-BundlePath',$Case.Bundle) }
    $arguments+=$Extra
    $job=[CortexReadonlyOwned.OwnedJob]::new(); $child=$null; $exitCode=$null
    try {
        $child=$job.Start($shell,[string[]]$arguments,$Case.Root,$log,$true,$variables)
        if (-not $child.Wait(15000)) { throw 'SOAK_SELFTEST_ENTRY_TIMEOUT' }
        $exitCode=$child.ExitCode
    }
    finally {
        try { Require-SoakSelfTest ($job.StopAndWait(10000) -and $job.ActiveCount -eq 0) 'headless harness 原 Job 未排空' }
        finally { $job.Dispose(); if ($null -ne $child) { $child.Dispose() } }
    }
    return [pscustomobject]@{Code=$exitCode;Output=$output;Owner=($output+'.owner.json');Log=$log}
}

try {
    $case=New-SoakFakeCase
    $validation=Invoke-SoakFakeEntry $case @('-ValidateOnly')
    $text=[IO.File]::ReadAllText($validation.Log)
    Require-SoakSelfTest ($validation.Code -eq 0 -and $text.Contains('validated_only') -and $text.Contains('"ownerVerified":  false')) 'ValidateOnly 不得宣称 owner 通过'
    Require-SoakSelfTest (-not (Test-Path -LiteralPath $validation.Output) -and -not (Test-Path -LiteralPath $validation.Owner) -and
        @(Get-ChildItem -LiteralPath $case.Root -Directory -Filter 'result-*.run-*').Count -eq 0) 'ValidateOnly 不得生成输出或运行目录'
    $existing=Invoke-SoakFakeEntry $case @('-ValidateOnly') -ExistingOutput
    Require-SoakSelfTest ($existing.Code -ne 0 -and ([IO.File]::ReadAllText($existing.Log)).Contains('SOAK_OUTPUT_EXISTS') -and
        [IO.File]::ReadAllText($existing.Output) -ceq '保留用户已有结果') '旧 OutputPath 必须原样保留'
    $directory=Invoke-SoakFakeEntry $case @('-ValidateOnly') -DirectoryOutput
    Require-SoakSelfTest ($directory.Code -eq 0 -and -not [IO.Directory]::Exists([IO.Path]::GetDirectoryName($directory.Output))) 'OutputDirectory 的只读校验不能创建目录'
    $case.OutputRoot=Join-Path $case.Fixture 'results\final-run-fake'
    $null=[IO.Directory]::CreateDirectory($case.OutputRoot)
    $callerLayout=Invoke-SoakFakeEntry $case @('-ValidateOnly')
    Require-SoakSelfTest ($callerLayout.Code -eq 0 -and -not [IO.File]::Exists($callerLayout.Output)) '必须兼容主线 final-run 的 fixture/results 全新结果布局'
    $case.OutputRoot=$case.Root
    $missing=Invoke-SoakFakeEntry $case @() $true
    Require-SoakSelfTest ($missing.Code -ne 0 -and ([IO.File]::ReadAllText($missing.Log)).Contains('SOAK_BUNDLE_REQUIRED')) '缺 bundle 必须明确说明接口'
    foreach ($mode in @('exit7','short','source-change')) {
        $runCase=if ($mode -ceq 'exit7') {$case} else {New-SoakFakeCase $mode}
        $run=Invoke-SoakFakeEntry $runCase
        $owner=Read-CortexSoakJson $run.Owner; $probe=Read-CortexSoakJson ($run.Output+'.probe.json')
        Require-SoakSelfTest ($run.Code -eq $(if($mode -ceq 'exit7'){7}else{1}) -and $owner.status -ceq 'failed' -and -not $owner.ownerVerified) '不得把假结果或非零退出记为 PASS'
        Require-SoakSelfTest ($owner.runner.originalJobDrained -and $owner.runner.activeAfterDrain -eq 0 -and
            $owner.runner.powerRequestAcquired -and $owner.runner.powerRequestReleased -and -not $owner.runner.powerRequestActive -and
            $owner.runner.childPid -gt 0) '原 Job 排空、电源释放和 PID 必须有终态证据'
        Require-SoakSelfTest ($owner.originalInputsUnchanged -and $owner.frozenInputsUnchanged -and $owner.toolSourcesUnchanged -and
            $probe.frozenWriteBlocked -and $probe.nonce -ceq $owner.nonce) '冻结输入必须锁定并绑定同一个 nonce'
        $expectedPath=(Join-Path $env:SystemRoot 'System32')+';'+[IO.Path]::GetDirectoryName($shell)
        Require-SoakSelfTest ($probe.path -ieq $expectedPath -and $null -eq $probe.owner) '不得继承任意 PATH 或 external owner 环境'
        $expectedJson=ConvertTo-Json -InputObject $runCase.Result -Depth 20 -Compress
        Require-SoakSelfTest ([IO.File]::ReadAllText($run.Output) -ceq $expectedJson) 'OutputPath 必须保持 portable 原始 JSON'
        $heartbeat=Read-CortexSoakJson $owner.heartbeatPath
        Require-SoakSelfTest ($heartbeat.phase -ceq 'terminal' -and $heartbeat.nonce -ceq $owner.nonce -and
            [IO.File]::Exists((Join-Path $owner.runDirectory 'launch.json'))) '心跳与 launch 必须有同轮审计'
        if ($mode -ceq 'short') { Require-SoakSelfTest (($owner.failures -join ' ').Contains('SOAK_PORTABLE_DURATION_GATE')) '短命假 runner 不能冒充跑满 60 秒' }
        if ($mode -ceq 'source-change') { Require-SoakSelfTest (-not $owner.sourceInputsUnchanged -and ($owner.failures -join ' ').Contains('SOAK_SOURCE_CHANGED')) '必须拒绝运行中源码变化' }
    }
    $case=New-SoakFakeCase
    $inputLocks=[Collections.Generic.List[IO.FileStream]]::new()
    try {
        $verified=Get-CortexSoakBundle $case.Bundle $case.Workspace (Join-Path $case.Fixture 'portable-second-host-smoke.ps1') $inputLocks
        Expect-SoakReject { [IO.File]::AppendAllText((Join-Path $case.Bundle 'cortex-local-tests.exe'),'变化') } ''
    }
    finally { foreach($stream in $inputLocks){$stream.Dispose()} }
    # 判据的正样本全是内存构造，不计为真实 owner/soak 证据。
    $syntheticRunner=[ordered]@{childExitCode=0;timedOut=$false;originalJobDrained=$true;activeAfterDrain=0;
        powerRequestAcquired=$true;powerRequestActive=$false;powerRequestReleased=$true;cleanupErrors=@();elapsedMilliseconds=61000L}
    Assert-CortexSoakPortableResult ([pscustomobject]$case.Result) $verified 60 5000 $syntheticRunner
    $checks++
    foreach($field in @('totalRunLines','totalSummaryLines','namedPassedLines')) {
        $altered=ConvertFrom-Json (ConvertTo-Json -InputObject $case.Result -Depth 20)
        $altered.tests.soak.harness.$field=2
        Expect-SoakReject { Assert-CortexSoakPortableResult $altered $verified 60 5000 $syntheticRunner } 'SOAK_PORTABLE_SINGLE_TEST_GATE'
    }
    $bad=Join-Path $root 'concatenated.json'
    Write-SoakFakeFile $bad '{"status":"passed"}{"status":"passed"}'
    Expect-SoakReject {Read-CortexSoakJson $bad} ''
    [IO.File]::AppendAllText((Join-Path $case.Bundle 'cortex-local-tests.exe'),'变化')
    $tampered=Invoke-SoakFakeEntry $case @('-ValidateOnly')
    Require-SoakSelfTest ($tampered.Code -ne 0 -and ([IO.File]::ReadAllText($tampered.Log)).Contains('SOAK_INPUT_HASH_MISMATCH')) '篡改 bundle 必须在启动前拒绝'
    $case=New-SoakFakeCase
    [IO.File]::AppendAllText($case.Source,'变化')
    $stale=Invoke-SoakFakeEntry $case @('-ValidateOnly')
    Require-SoakSelfTest ($stale.Code -ne 0 -and ([IO.File]::ReadAllText($stale.Log)).Contains('SOAK_PROOF_SOURCE_STALE')) '过时本地 proof 必须拒绝'

    # 只在函数级把电源请求换成内存计数；监督进程和 Job 仍是真实的。
    $powerEvents=[Collections.Generic.List[bool]]::new()
    function Set-CortexSoakPowerRequest { param([bool]$Enabled) $powerEvents.Add($Enabled) }
    $jobRoot=Join-Path $root 'timeout'; $null=[IO.Directory]::CreateDirectory($jobRoot)
    $variables=New-CortexSoakEnvironment $jobRoot $shell 'headless-timeout'
    foreach ($startupFailure in @($false,$true)) {
        $state=[ordered]@{childPid=$null;childExitCode=$null;startedAt=$null;finishedAt=$null;elapsedMilliseconds=0L;
            timedOut=$false;originalJobDrained=$false;activeBeforeDrain=$null;activeAfterDrain=$null;
            powerRequestAcquired=$false;powerRequestActive=$false;powerRequestReleased=$false;
            cleanupErrors=[Collections.Generic.List[string]]::new()}
        $log=Join-Path $jobRoot ('child-'+$startupFailure+'.log')
        $exe=if($startupFailure){Join-Path $jobRoot 'missing.exe'}else{$shell}
        $failed=$false
        try { Invoke-CortexSoakOwnedRunner $exe @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120') $jobRoot $log $variables $jobHelper 1000 $state {param($state)} }
        catch {$failed=$true}
        Require-SoakSelfTest ($failed -and $state.originalJobDrained -and $state.activeAfterDrain -eq 0 -and $state.powerRequestReleased) '超时/启动失败也必须清退原 Job 并释放电源请求'
        if(-not $startupFailure){Require-SoakSelfTest $state.timedOut 'deadline 必须终止本次受控 child'}
    }
    Require-SoakSelfTest (($powerEvents.ToArray() -join ',') -ceq 'True,False,True,False') '电源请求必须逐次成对释放'
    [ordered]@{status='headless_selftest_passed';checks=$checks;scope='fake_files_commands_and_controlled_jobs_only';
        guiStarted=$false;driverStarted=$false;cargoStarted=$false;realSoakExecuted=$false;runtimeOwnershipVerified=$false;
        directory=$root} | ConvertTo-Json
}
finally { $env:CARGO_HOME=$previousCargoHome }
