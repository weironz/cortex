# 只复用来源记录的纯读取函数；不能调用会执行 cargo metadata/build 的入口。
. (Join-Path $PSScriptRoot 'test-artifact-provenance.ps1')

function Assert-CortexSoakShell {
    $windows = [Environment]::GetFolderPath([Environment+SpecialFolder]::Windows)
    $shell = Join-Path $windows 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $current = [Diagnostics.Process]::GetCurrentProcess()
    try { $executable = $current.MainModule.FileName }
    finally { $current.Dispose() }
    if ($PSVersionTable.PSVersion.Major -ne 5 -or $PSVersionTable.PSVersion.Minor -ne 1 -or
        -not [Environment]::Is64BitProcess -or $executable -ine $shell) {
        throw 'SOAK_SYSTEM_PS51_REQUIRED：请用系统 64 位 Windows PowerShell 5.1 -NoProfile -File soak.ps1 运行；不接受 PS7 的来源摘要排序。'
    }
    return $shell
}

function Resolve-CortexSoakPath {
    param([string]$Path, [bool]$MustExist = $false, [bool]$Directory = $false)
    if ([string]::IsNullOrWhiteSpace($Path) -or $Path -notmatch '^[A-Za-z]:\\' -or
        $Path.IndexOfAny([char[]]";`r`n`0") -ge 0 -or $Path.Substring(2).Contains(':')) {
        throw 'SOAK_LOCAL_PATH_REQUIRED：必须提供本地磁盘绝对路径，不能包含流名或 allowlist 分隔符。'
    }
    $full = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    $drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($full))
    if ($drive.DriveType -notin @([IO.DriveType]::Fixed, [IO.DriveType]::Removable, [IO.DriveType]::Ram)) {
        throw 'SOAK_LOCAL_DISK_REQUIRED'
    }
    Assert-CortexArtifactNoReparse $full
    if ($MustExist -and (($Directory -and -not [IO.Directory]::Exists($full)) -or
        (-not $Directory -and -not [IO.File]::Exists($full)))) { throw "SOAK_INPUT_MISSING：$full" }
    return $full
}

function Read-CortexSoakJson {
    param([string]$Path)
    $value = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($Path, [Text.UTF8Encoding]::new($false, $true)))
    if ($null -eq $value -or $value -is [array] -or $value -isnot [pscustomobject]) { throw 'SOAK_JSON_OBJECT_REQUIRED' }
    return $value
}

function Open-CortexSoakInput {
    param([string]$Path, [string]$Expected, $Locks)
    $null = Resolve-CortexSoakPath $Path $true
    # 锁原输入和执行副本，禁止校验后写入、替换或删除；直到原 Job 排空才释放。
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $Locks.Add($stream)
    $hash = Get-CortexArtifactSha256 $Path
    if ($Expected -and ($Expected -cnotmatch '^[0-9a-f]{64}$' -or $hash -cne $Expected)) {
        throw "SOAK_INPUT_HASH_MISMATCH：$Path"
    }
    return $hash
}

function Get-CortexSoakSources {
    param($Context, [string]$RepositoryRoot)
    # 当前虚拟 workspace 的本地包都在 crates；枚举而非相信 proof 自报的子集。
    # 不另写 Cargo 解析器，不启动 Cargo，也不猜测原构建目录里的 exe。
    if ($Context.repositoryRoot -ine $RepositoryRoot -or
        $Context.manifestPath -ine (Join-Path $RepositoryRoot 'crates\cortex-local\Cargo.toml') -or
        $Context.sourcePath -ine (Join-Path $RepositoryRoot 'crates\cortex-local\src\main.rs')) {
        throw 'SOAK_PROOF_SOURCE_ROOT_MISMATCH'
    }
    $roots = @(Get-ChildItem -LiteralPath (Join-Path $RepositoryRoot 'crates') -Directory | Where-Object {
        [IO.File]::Exists((Join-Path $_.FullName 'Cargo.toml'))
    } | ForEach-Object { $_.FullName } | Sort-Object)
    if ($roots.Count -eq 0 -or (Get-CortexArtifactDigest $roots) -cne
        (Get-CortexArtifactDigest @($Context.sourceRoots | Sort-Object))) { throw 'SOAK_PROOF_PACKAGE_ROOTS_MISMATCH' }
    return Get-CortexArtifactInputs $Context
}

function Get-CortexSoakBundle {
    param([string]$BundlePath, [string]$RepositoryRoot, [string]$PortableRunner, $Locks)
    $bundle = Resolve-CortexSoakPath $BundlePath $true $true
    $manifestPath = Join-Path $bundle 'SHA256SUMS.json'
    $rows = [Collections.Generic.List[object]]::new()
    $manifestHash = Open-CortexSoakInput $manifestPath '' $Locks
    $rows.Add([ordered]@{ relativePath='SHA256SUMS.json'; path=$manifestPath; sha256=$manifestHash })
    $manifest = Read-CortexSoakJson $manifestPath
    if ($manifest.schemaVersion -ne 1) { throw 'SOAK_BUNDLE_SCHEMA' }
    $files = [ordered]@{
        'cortex-local-tests.exe' = [string]$manifest.sha256.testExe
        'test-artifact-proof.json' = [string]$manifest.sha256.testArtifactProof
        'cargo-messages.ndjson' = [string]$manifest.sha256.testArtifactNdjson
        'cortex-computer-use-soak-fixture.exe' = [string]$manifest.sha256.fixtureExe
        'vcruntime140.dll' = [string]$manifest.sha256.vcRuntime
        'portable-second-host-smoke.ps1' = [string]$manifest.sha256.portableRunner
    }
    # 只复制 portable 会消费的文件；安装包、e2e 与其他未校验 DLL 不进入执行目录。
    $cuaNames = @('cua-driver.exe', 'cua-driver-uia.exe', 'cua-cursor-theme.exe', 'cua_driver_sdk.dll',
        'cua_driver_node_runtime.node', 'cua_driver_abi.h', 'LICENSE.md', 'THIRD_PARTY_NOTICES.md', 'SBOM.spdx.json', 'README.md')
    foreach ($name in $cuaNames) { $files['cua\' + $name] = [string]$manifest.sha256.cua.$name }
    foreach ($relative in $files.Keys) {
        if ($files[$relative] -cnotmatch '^[0-9a-f]{64}$') { throw "SOAK_BUNDLE_HASH_REQUIRED：$relative" }
        $path = Join-Path $bundle $relative
        $hash = Open-CortexSoakInput $path $files[$relative] $Locks
        $rows.Add([ordered]@{ relativePath=$relative; path=$path; sha256=$hash })
    }
    if ($files['portable-second-host-smoke.ps1'] -cne (Get-CortexArtifactSha256 $PortableRunner)) {
        throw 'SOAK_PORTABLE_RUNNER_STALE：bundle 必须携带当前 portable runner。'
    }
    $proof = Read-CortexSoakJson (Join-Path $bundle 'test-artifact-proof.json')
    if ($proof.schemaVersion -ne 1 -or $proof.purpose -cne 'local-cargo-test-artifact-provenance' -or
        $proof.target -cne 'x86_64-pc-windows-msvc' -or $proof.profile -cne 'release' -or
        $proof.cargoExitCode -ne 0 -or (Get-CortexArtifactDigest @($proof.cargoArguments)) -cne
        (Get-CortexArtifactDigest @(Get-CortexArtifactBuildArguments)) -or
        $proof.buildEnvironmentBefore -cnotmatch '^[0-9a-f]{64}$' -or
        $proof.buildEnvironmentBefore -cne $proof.buildEnvironmentAfter) { throw 'SOAK_PROOF_BUILD_CONTRACT' }
    foreach ($snapshot in @($proof.inputsBefore, $proof.inputsAfter)) {
        if ($snapshot.sha256 -cne (Get-CortexArtifactDigest @($snapshot.files))) { throw 'SOAK_PROOF_INPUT_DIGEST' }
    }
    if ($proof.inputsBefore.sha256 -cne $proof.inputsAfter.sha256 -or
        $manifest.testArtifact.sourceInputsSha256 -cne $proof.inputsAfter.sha256 -or
        $manifest.testArtifact.target -cne $proof.target -or $manifest.testArtifact.profile -cne $proof.profile -or
        $manifest.version -cne $proof.context.version -or $files['cortex-local-tests.exe'] -cne $proof.testExeSha256 -or
        $files['cargo-messages.ndjson'] -cne $proof.ndjsonSha256) { throw 'SOAK_PROOF_BUNDLE_BINDING' }
    $artifact = Read-CortexTestCompilerArtifact (Join-Path $bundle 'cargo-messages.ndjson') $proof.context
    if ((Get-CortexArtifactDigest $artifact) -cne (Get-CortexArtifactDigest $proof.artifact) -or
        $artifact.fresh -ne $manifest.testArtifact.cargoFresh) { throw 'SOAK_PROOF_COMPILER_ARTIFACT' }
    $sources = Get-CortexSoakSources $proof.context $RepositoryRoot
    if ($sources.sha256 -cne $proof.inputsAfter.sha256) {
        throw 'SOAK_PROOF_SOURCE_STALE：当前 Rust/Cargo 输入与 bundle proof 不同；请由主线提供当前构建验收包，本入口不会重编。'
    }
    return [pscustomobject]@{ Root=$bundle; Manifest=$manifest; Proof=$proof; Rows=$rows.ToArray(); Sources=$sources }
}

function Get-CortexSoakPortableArguments {
    param([string]$Bundle, $Manifest, [string]$OutputPath, [int]$Seconds, [int]$IntervalMs)
    return @('-TestExePath', (Join-Path $Bundle 'cortex-local-tests.exe'),
        '-FixtureExePath', (Join-Path $Bundle 'cortex-computer-use-soak-fixture.exe'),
        '-VCRuntimePath', (Join-Path $Bundle 'vcruntime140.dll'), '-CuaDirectory', (Join-Path $Bundle 'cua'),
        '-ExpectedTestSha256', [string]$Manifest.sha256.testExe,
        '-ExpectedFixtureSha256', [string]$Manifest.sha256.fixtureExe,
        '-ExpectedVCRuntimeSha256', [string]$Manifest.sha256.vcRuntime,
        '-OutputPath', $OutputPath, '-Seconds', [string]$Seconds, '-IntervalMs', [string]$IntervalMs)
}

function New-CortexSoakPrivateDirectory {
    param([string]$Path)
    $null = Resolve-CortexSoakPath $Path
    if (Test-Path -LiteralPath $Path) { throw "SOAK_OUTPUT_EXISTS：$Path" }
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($sid in @([Security.Principal.WindowsIdentity]::GetCurrent().User,
        [Security.Principal.SecurityIdentifier]::new('S-1-5-18'))) {
        $rule = [Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl',
            'ContainerInherit,ObjectInherit', 'None', 'Allow')
        $acl.AddAccessRule($rule)
    }
    $null = [IO.Directory]::CreateDirectory($Path, $acl)
}

function Write-CortexSoakJson {
    param([IO.Stream]$Stream, $Value)
    $bytes = [Text.Encoding]::UTF8.GetBytes((ConvertTo-Json -InputObject $Value -Depth 40) + "`n")
    $Stream.Position = 0
    $Stream.SetLength(0)
    $Stream.Write($bytes, 0, $bytes.Length)
    $Stream.Flush()
}

function Assert-CortexSoakUnchanged {
    param($Rows)
    foreach ($row in $Rows) {
        if ((Get-CortexArtifactSha256 $row.path) -cne $row.sha256) { throw "SOAK_INPUT_CHANGED：$($row.path)" }
    }
}

function New-CortexSoakEnvironment {
    param([string]$Root, [string]$Shell, [string]$Nonce)
    $variables = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::OrdinalIgnoreCase)
    $windows = [Environment]::GetFolderPath([Environment+SpecialFolder]::Windows)
    $shellRoot = [IO.Path]::GetDirectoryName($Shell)
    $variables['SystemRoot'] = $windows; $variables['WINDIR'] = $windows
    $variables['PATH'] = (Join-Path $windows 'System32') + ';' + $shellRoot
    $variables['PSModulePath'] = Join-Path $shellRoot 'Modules'
    foreach ($name in @('tmp', 'profile', 'appdata', 'localappdata')) {
        $null = [IO.Directory]::CreateDirectory((Join-Path $Root $name))
    }
    $variables['TEMP'] = Join-Path $Root 'tmp'; $variables['TMP'] = $variables['TEMP']
    $variables['USERPROFILE'] = Join-Path $Root 'profile'
    $variables['APPDATA'] = Join-Path $Root 'appdata'; $variables['LOCALAPPDATA'] = Join-Path $Root 'localappdata'
    $variables['CORTEX_SOAK_RUN_NONCE'] = $Nonce
    return ,$variables
}

function Set-CortexSoakPowerRequest {
    param([bool]$Enabled)
    if (-not ('CortexSoakPowerRequest' -as [type])) {
        Add-Type -TypeDefinition @'
using System.Runtime.InteropServices;
public static class CortexSoakPowerRequest {
    [DllImport("kernel32.dll", SetLastError=true)] public static extern uint SetThreadExecutionState(uint flags);
}
'@
    }
    $flags = if ($Enabled) { [uint32]2147483651 } else { [uint32]2147483648 }
    if ([CortexSoakPowerRequest]::SetThreadExecutionState($flags) -eq 0) { throw 'SOAK_POWER_REQUEST_FAILED' }
}

function Invoke-CortexSoakOwnedRunner {
    param([string]$Shell, [string[]]$Arguments, [string]$Root, [string]$LogPath, $Environment,
        [string]$JobHelper, [int]$TimeoutMs, $Record, [scriptblock]$Heartbeat)
    if (-not ('CortexReadonlyOwned.OwnedJob' -as [type])) { Add-Type -Path $JobHelper }
    $job = $null; $child = $null; $powerAttempted = $false
    $clock = [Diagnostics.Stopwatch]::StartNew()
    try {
        # 即使电源请求或进程创建失败，也走同一个 finally；清理错误不得遮掉原始退出码。
        $powerAttempted = $true
        Set-CortexSoakPowerRequest $true
        $Record.powerRequestAcquired = $true
        $Record.powerRequestActive = $true
        $job = [CortexReadonlyOwned.OwnedJob]::new()
        $child = $job.Start($Shell, $Arguments, $Root, $LogPath, $true, $Environment)
        $Record.childPid = $child.Id
        $Record.startedAt = [DateTime]::UtcNow.ToString('o')
        $nextHeartbeat = 0L
        do {
            if ($clock.ElapsedMilliseconds -ge $nextHeartbeat) {
                $Record.elapsedMilliseconds = $clock.ElapsedMilliseconds
                & $Heartbeat $Record
                $nextHeartbeat = $clock.ElapsedMilliseconds + 5000
            }
            if ($child.Wait(250)) { $Record.childExitCode = $child.ExitCode; break }
            if ($clock.ElapsedMilliseconds -ge $TimeoutMs) {
                $Record.timedOut = $true
                throw 'SOAK_RUNNER_HARD_DEADLINE'
            }
        } while ($true)
    }
    finally {
        $Record.elapsedMilliseconds = $clock.ElapsedMilliseconds
        $Record.finishedAt = [DateTime]::UtcNow.ToString('o')
        try {
            if ($null -ne $job) {
                $Record.activeBeforeDrain = $job.ActiveCount
                $Record.originalJobDrained = $job.StopAndWait(10000)
                $Record.activeAfterDrain = $job.ActiveCount
            }
        }
        catch { $Record.cleanupErrors.Add('SOAK_ORIGINAL_JOB_DRAIN_FAILED：' + $_.Exception.Message) }
        finally {
            if ($null -ne $job) { $job.Dispose() }
            if ($null -ne $child) { $child.Dispose() }
            if ($powerAttempted) {
                try {
                    Set-CortexSoakPowerRequest $false
                    $Record.powerRequestReleased = $true
                    $Record.powerRequestActive = $false
                }
                catch { $Record.cleanupErrors.Add('SOAK_POWER_RELEASE_FAILED：' + $_.Exception.Message) }
            }
        }
    }
}

function Assert-CortexSoakPortableResult {
    param($Result, $Bundle, [int]$Seconds, [int]$IntervalMs, $Runner)
    if ($Runner.childExitCode -ne 0 -or $Runner.timedOut -or -not $Runner.originalJobDrained -or
        $Runner.activeAfterDrain -ne 0 -or -not $Runner.powerRequestAcquired -or $Runner.powerRequestActive -or
        -not $Runner.powerRequestReleased -or $Runner.cleanupErrors.Count -ne 0) {
        throw 'SOAK_OWNER_TERMINAL_GATE'
    }
    if ($Result.schemaVersion -ne 1 -or $Result.suite -cne 'cortex-computer-use-windows-portable-second-host-smoke' -or
        $Result.status -cne 'passed' -or $Result.runtimeOwnership -cne 'agent' -or
        $Result.requestedSeconds -ne $Seconds -or $Result.intervalMs -ne $IntervalMs) { throw 'SOAK_PORTABLE_RESULT_GATE' }
    $names = @{
        secure='computer::real::tests::cua_secure_fixture_requires_handoff_without_input'
        soak='computer::real::tests::cua_fixture_long_running_soak'
    }
    foreach ($phase in @('secure', 'soak')) {
        $test = $Result.tests.$phase
        if ($test.name -cne $names[$phase] -or $test.status -cne 'passed' -or $test.exitCode -ne 0 -or
            $test.harness.exitCode -ne 0 -or $test.harness.passed -isnot [bool] -or -not $test.harness.passed) {
            throw 'SOAK_PORTABLE_TEST_GATE'
        }
        foreach ($field in @('runningOneLines', 'namedPassedLines', 'onePassedSummaryLines', 'totalRunLines', 'totalSummaryLines')) {
            if ($test.harness.$field -ne 1) { throw 'SOAK_PORTABLE_SINGLE_TEST_GATE' }
        }
    }
    if ($Result.tests.soak.durationMilliseconds -lt ([long]$Seconds * 1000) -or
        $Runner.elapsedMilliseconds -lt $Result.tests.soak.durationMilliseconds) { throw 'SOAK_PORTABLE_DURATION_GATE' }
    foreach ($binding in @(@('test','testExe'), @('fixture','fixtureExe'), @('vcRuntime','vcRuntime'))) {
        $item = $Result.inputs.($binding[0]); $expected = $Bundle.Manifest.sha256.($binding[1])
        if ($item.sha256 -cne $expected -or $item.expectedSha256 -cne $expected -or $item.executionSha256 -cne $expected) {
            throw 'SOAK_PORTABLE_INPUT_BINDING'
        }
    }
    foreach ($name in @('cua-driver.exe', 'cua-driver-uia.exe', 'cua-cursor-theme.exe', 'cua_driver_sdk.dll',
        'cua_driver_node_runtime.node', 'cua_driver_abi.h')) {
        $item = $Result.inputs.cua.$name
        if ($item.sha256 -cne $Bundle.Manifest.sha256.cua.$name -or $item.executionSha256 -cne $item.sha256) {
            throw 'SOAK_PORTABLE_CUA_BINDING'
        }
    }
}
