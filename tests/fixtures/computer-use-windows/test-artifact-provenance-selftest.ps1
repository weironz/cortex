<#
.SYNOPSIS
无需构建和 GUI 的来源记录回归。所有 Cargo 调用由函数替身接管，exe 仅是文本假文件。
.EXAMPLE
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\test-artifact-provenance-selftest.ps1
#>
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. ([IO.Path]::Combine($PSScriptRoot, 'test-artifact-provenance.ps1'))
$script:caseNumber = 0
$script:passed = 0
$script:testRoot = Join-Path ([IO.Path]::GetTempPath()) ('cortex-artifact-regression-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $script:testRoot
$script:originalRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
$script:previousCargoHome = $env:CARGO_HOME
$script:previousCargoFunction = Get-Item -LiteralPath Function:global:cargo -ErrorAction SilentlyContinue
$script:previousState = Get-Variable -Name CortexArtifactRegressionState -Scope Global -ErrorAction SilentlyContinue
$env:CARGO_HOME = Join-Path $script:testRoot 'isolated-cargo-home'

function Write-FakeFile {
    param([string]$Path, [string]$Text = '假文件，严禁执行')
    $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path))
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false))
}

function New-FakeWorkspace {
    $script:caseNumber++
    $caseRoot = Join-Path $script:testRoot ('case-' + $script:caseNumber)
    $root = Join-Path $caseRoot '源码 space'
    $fixtureRoot = Join-Path $root 'tests\fixtures\computer-use-windows'
    $manifest = Join-Path $root 'crates\cortex-local\Cargo.toml'
    $source = Join-Path $root 'crates\cortex-local\src\main.rs'
    Write-FakeFile (Join-Path $root 'Cargo.toml') "[workspace]`n[workspace.package]`nversion = `"9.8.7`"`n"
    Write-FakeFile (Join-Path $root 'Cargo.lock') '# 假锁文件'
    Write-FakeFile $manifest "[package]`nname = `"cortex-local`"`nversion.workspace = true`n"
    Write-FakeFile $source 'fn main() {} // 中文路径与 NDJSON'
    Write-FakeFile (Join-Path $root 'crates\cortex-local\tests\data.sql') 'SELECT 1;'
    Write-FakeFile (Join-Path $root 'crates\helper\Cargo.toml') '[package]'
    Write-FakeFile (Join-Path $root 'crates\helper\src\lib.rs') '// 依赖输入'
    Write-FakeFile (Join-Path $root 'migrations\001.sql') 'SELECT 1;'
    Write-FakeFile (Join-Path $root '.cargo\config.toml') '[build]'
    Write-FakeFile (Join-Path $root 'scripts\windows\native-json.ps1') ''
    Copy-Item -LiteralPath (Join-Path $script:originalRoot 'scripts\windows\native-json.ps1') `
        -Destination (Join-Path $root 'scripts\windows\native-json.ps1')
    $targetRoot = Join-Path $caseRoot '实际 Cargo target'
    $exe = Join-Path $targetRoot 'x86_64-pc-windows-msvc\release\deps\cortex_local-current_test.exe'
    Write-FakeFile $exe
    $target = [ordered]@{ name = 'cortex-local'; kind = @('bin'); crate_types = @('bin'); src_path = $source }
    $packageId = 'path+file:///fake/cortex-local#9.8.7'
    $metadata = [ordered]@{
        workspace_root = $root
        target_directory = $targetRoot
        packages = @(
            [ordered]@{ name = 'cortex-local'; version = '9.8.7'; id = $packageId; source = $null;
                manifest_path = $manifest; targets = @($target) },
            [ordered]@{ name = 'helper'; version = '1.0.0'; id = 'path+file:///fake/helper#1.0.0'; source = $null;
                manifest_path = (Join-Path $root 'crates\helper\Cargo.toml'); targets = @() }
        )
    }
    $artifact = [ordered]@{
        reason = 'compiler-artifact'; package_id = $packageId; manifest_path = $manifest; target = $target
        profile = [ordered]@{ test = $true; opt_level = '3'; debug_assertions = $false }
        executable = $exe; filenames = @($exe); fresh = $false; features = @()
    }
    $global:CortexArtifactRegressionState = [ordered]@{
        Root = $root; FixtureRoot = $fixtureRoot; CaseRoot = $caseRoot; Exe = $exe
        Metadata = $metadata; Artifact = $artifact; BuildExitCode = 0; Mutation = $null
        CargoCalls = (New-Object 'System.Collections.Generic.List[string]')
    }
    $null = [IO.Directory]::CreateDirectory($fixtureRoot)
    foreach ($name in @('build-second-host-bundle.ps1', 'test-artifact-provenance.ps1')) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination (Join-Path $fixtureRoot $name)
    }
    foreach ($name in @('acceptance-report.ps1', 'portable-second-host-smoke.ps1', 'run-second-host-acceptance.ps1',
        'interactive-desktop.ps1', 'interactive-desktop-selftest.ps1',
        'e2e.ps1', 'cleanup-artifacts.ps1', 'target\cortex-computer-use-windows-fixture.exe',
        'chrome\acceptance.html', 'chrome\page2.html')) { Write-FakeFile (Join-Path $fixtureRoot $name) }
    foreach ($name in @('fixture.exe', 'vcruntime140.dll', 'installer.exe', 'installer.exe.cdx.json')) {
        Write-FakeFile (Join-Path $caseRoot $name)
    }
    foreach ($name in @('cua-driver.exe', 'cua-driver-uia.exe', 'cua-cursor-theme.exe', 'cua_driver_sdk.dll',
        'cua_driver_node_runtime.node', 'cua_driver_abi.h', 'LICENSE.md', 'THIRD_PARTY_NOTICES.md', 'SBOM.spdx.json', 'README.md')) {
        Write-FakeFile (Join-Path $caseRoot ('cua\' + $name))
    }
    return $global:CortexArtifactRegressionState
}

try {
# 函数优先于 PATH 中的同名程序。替身只接收约定命令，任何 --list/执行企图都会失败。
function global:cargo {
    $arguments = @($args)
    $state = $global:CortexArtifactRegressionState
    $state.CargoCalls.Add(($arguments -join ' '))
    $global:LASTEXITCODE = 0
    if ($arguments[0] -ceq 'metadata') {
        return ConvertTo-Json -InputObject $state.Metadata -Depth 30 -Compress
    }
    if ((Get-CortexArtifactDigest $arguments) -cne (Get-CortexArtifactDigest @(Get-CortexArtifactBuildArguments))) {
        throw '测试检测到非约定的 Cargo 命令；禁止启动任何真实构建或 exe。'
    }
    if ($null -ne $state.Mutation) { & $state.Mutation $state }
    '过程宏普通输出，不是 JSON'
    ConvertTo-Json -InputObject $state.Artifact -Depth 30 -Compress
    '{"reason":"build-finished","success":true}'
    $global:LASTEXITCODE = $state.BuildExitCode
}

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function Assert-Rejected {
    param([scriptblock]$Action, [string]$Pattern)
    try { & $Action | Out-Null }
    catch {
        if ($_.Exception.Message -notmatch $Pattern) { throw "失败原因不符：$($_.Exception.Message)；预期 $Pattern" }
        return
    }
    throw "本应拒绝：$Pattern"
}

function Test-Case {
    param([string]$Name, [scriptblock]$Action)
    & $Action
    $script:passed++
    Write-Output "PASS $Name"
}

function New-FakeProof {
    param($State)
    return New-CortexTestArtifactProof $State.Root (Join-Path $State.CaseRoot 'proof')
}

function Save-FakeProof {
    param([string]$Path, $Value)
    Write-FakeFile $Path ((ConvertTo-Json -InputObject $Value -Depth 40) + "`n")
}

function Get-FakeBundleArguments {
    param($State, $Build)
    return @{
        TestExePath = $Build.testExePath; TestArtifactProofPath = $Build.testArtifactProofPath
        FixtureExePath = (Join-Path $State.CaseRoot 'fixture.exe')
        VCRuntimePath = (Join-Path $State.CaseRoot 'vcruntime140.dll')
        InstallerPath = (Join-Path $State.CaseRoot 'installer.exe')
        CuaDirectory = (Join-Path $State.CaseRoot 'cua')
        OutputDirectory = (Join-Path $State.Root 'dist\safe-bundle')
    }
}

Test-Case '系统模块从 PSHOME 路径加载，兼容 Git Bash 的 PSModulePath' {
    foreach ($name in @('Microsoft.PowerShell.Utility', 'Microsoft.PowerShell.Management', 'Microsoft.PowerShell.Security')) {
        $modules = @(Get-Module -Name $name)
        $expected = [IO.Path]::Combine($PSHOME, 'Modules', $name, "$name.psd1")
        Assert-True ($modules.Count -eq 1 -and $modules[0].Path -eq $expected) "必须使用当前 PowerShell 的系统模块：$name"
    }
}

Test-Case '重定向 target、中文 NDJSON、旧 mtime 干扰与可复用缓存' {
    $state = New-FakeWorkspace
    $old = Join-Path $state.Root 'target\release\deps\cortex_local-old.exe'
    Write-FakeFile $old '旧 exe，禁止选取'
    (Get-Item -LiteralPath $old).LastWriteTime = [DateTime]::Now.AddYears(1)
    $state.Artifact.fresh = $true
    $build = New-FakeProof $state
    $proof = Assert-CortexTestArtifactProof $state.Root $state.Exe $build.testArtifactProofPath
    Assert-True ($proof.artifact.executable -eq $state.Exe -and $proof.artifact.fresh) '必须绑定本次 Cargo 返回的缓存产物。'
    Assert-True ($proof.inputsBefore.sha256 -ceq $proof.inputsAfter.sha256) '编译前后输入必须相同。'
}

Test-Case '打包时 target 环境改变仍绑定 proof 的明确原 exe' {
    $state = New-FakeWorkspace
    $build = New-FakeProof $state
    $state.Metadata.target_directory = Join-Path $state.CaseRoot 'different-current-target'
    $proof = Assert-CortexTestArtifactProof $state.Root $state.Exe $build.testArtifactProofPath
    Assert-True ($proof.artifact.executable -eq $state.Exe) '不能用当前 target 环境改选 exe。'
}

Test-Case '混合 NDJSON 只选择指定包 bin profile.test=true 的产物' {
    $state = New-FakeWorkspace
    $context = Get-CortexArtifactContext $state.Root
    $normal = ConvertFrom-Json (ConvertTo-Json -InputObject $state.Artifact -Depth 20)
    $normal.profile.test = $false
    $other = ConvertFrom-Json (ConvertTo-Json -InputObject $state.Artifact -Depth 20)
    $other.package_id = 'other-package'
    $log = Join-Path $state.CaseRoot 'mixed.ndjson'
    $lines = @('宏普通输出', (ConvertTo-Json $normal -Depth 20 -Compress),
        (ConvertTo-Json $other -Depth 20 -Compress), (ConvertTo-Json $state.Artifact -Depth 20 -Compress),
        '{"reason":"build-finished","success":true}')
    Write-FakeFile $log ($lines -join "`n")
    $artifact = Read-CortexTestCompilerArtifact $log $context
    Assert-True ($artifact.executable -eq $state.Exe -and $artifact.profile.test) '无关 compiler-artifact 不得污染选择。'
}

Test-Case '真实 builder 在假 workspace 打包 exe、proof、NDJSON 和两个桌面模块文件' {
    $state = New-FakeWorkspace
    $build = New-FakeProof $state
    $arguments = Get-FakeBundleArguments $state $build
    $frozenFixture = Join-Path $state.FixtureRoot 'target\cortex-computer-use-windows-fixture.exe'
    $frozenHash = Get-CortexArtifactSha256 $frozenFixture
    # 仅移动临时假输入，证明显式新 fixture 不依赖那个冻结旧路径存在。
    $retainedFixture = Join-Path $state.CaseRoot 'retained-frozen-fixture.exe'
    Move-Item -LiteralPath $frozenFixture -Destination $retainedFixture
    $newFixture = Join-Path $state.CaseRoot 'new-fixture.exe'
    Write-FakeFile $newFixture '全新 fixture 假文件'
    $arguments.WindowsFixtureExePath = $newFixture
    $arguments.FixtureExePath = $newFixture
    & (Join-Path $state.FixtureRoot 'build-second-host-bundle.ps1') @arguments | Out-Null
    $manifest = ConvertFrom-Json ([IO.File]::ReadAllText((Join-Path $arguments.OutputDirectory 'SHA256SUMS.json')))
    Assert-True ($manifest.sha256.testExe -ceq (Get-CortexArtifactSha256 $state.Exe)) '打包 exe hash 必须与选中产物一致。'
    $newFixtureHash = Get-CortexArtifactSha256 $newFixture
    Assert-True ($manifest.sha256.fixtureExe -ceq $newFixtureHash -and $manifest.sha256.windowsFixtureExe -ceq $newFixtureHash) '普通和 soak fixture 必须都使用显式新文件。'
    Assert-True ((Get-CortexArtifactSha256 (Join-Path $arguments.OutputDirectory 'target\cortex-computer-use-windows-fixture.exe')) -ceq $newFixtureHash) '普通 E2E fixture 必须复制自 WindowsFixtureExePath。'
    Assert-True ((Get-CortexArtifactSha256 $retainedFixture) -ceq $frozenHash) '旧 fixture 内容必须保持不变。'
    foreach ($entry in @(
        @('interactiveDesktop', 'interactive-desktop.ps1'),
        @('interactiveDesktopSelftest', 'interactive-desktop-selftest.ps1')
    )) {
        $expected = Get-CortexArtifactSha256 (Join-Path $state.FixtureRoot $entry[1])
        Assert-True ($manifest.sha256.($entry[0]) -ceq $expected -and $manifest.hashes.($entry[0]) -ceq $expected) '桌面模块必须复制并记录两个接口的 hash。'
        Assert-True ((Get-CortexArtifactSha256 (Join-Path $arguments.OutputDirectory $entry[1])) -ceq $expected) '桌面模块仅复制，不改写。'
    }
    Assert-True (Test-Path -LiteralPath ($arguments.OutputDirectory + '.zip')) '应生成临时 bundle zip。'
    Assert-True (Test-Path -LiteralPath (Join-Path $arguments.OutputDirectory 'test-artifact-proof.json')) 'bundle 必须包含 proof。'
}

Test-Case '拒绝首次 proof 校验后、复制前被替换的 test exe' {
    $state = New-FakeWorkspace
    $build = New-FakeProof $state
    $arguments = Get-FakeBundleArguments $state $build
    $previousCopy = Get-Item -LiteralPath Function:global:Copy-Item -ErrorAction SilentlyContinue
    try {
        function global:Copy-Item {
            param([string]$LiteralPath, [string]$Destination)
            if ($LiteralPath -eq $global:CortexArtifactRegressionState.Exe) {
                Write-FakeFile $LiteralPath '模拟并行 Cargo 替换后的假 exe'
            }
            Microsoft.PowerShell.Management\Copy-Item -LiteralPath $LiteralPath -Destination $Destination
        }
        Assert-Rejected { & (Join-Path $state.FixtureRoot 'build-second-host-bundle.ps1') @arguments } '复制期间'
        Assert-True (-not (Test-Path -LiteralPath ($arguments.OutputDirectory + '.zip'))) 'staged exe 与 proof 不一致时不得生成 zip。'
    }
    finally {
        if ($null -ne $previousCopy) { Set-Item -LiteralPath Function:global:Copy-Item -Value $previousCopy.ScriptBlock }
        else { Remove-Item -LiteralPath Function:global:Copy-Item }
    }
}

Test-Case 'builder 缺参数时明确指向 producer，不探测默认 exe' {
    $state = New-FakeWorkspace
    Assert-Rejected { & (Join-Path $state.FixtureRoot 'build-second-host-bundle.ps1') } 'build-second-host-test-artifact.ps1'
    Assert-True ($state.CargoCalls.Count -eq 0) '缺参数不能启动 Cargo 或发现旧 exe。'
}

foreach ($mutationName in @('source', 'dependency', 'resource', 'lock', 'add', 'delete', 'config', 'migration', 'version')) {
    Test-Case "拒绝构建后的输入变化：$mutationName" {
        $state = New-FakeWorkspace
        $build = New-FakeProof $state
        switch ($mutationName) {
            'source' { Write-FakeFile $state.Artifact.target.src_path 'changed' }
            'dependency' { Write-FakeFile (Join-Path $state.Root 'crates\helper\src\lib.rs') 'changed' }
            'resource' { Write-FakeFile (Join-Path $state.Root 'crates\cortex-local\tests\data.sql') 'changed' }
            'lock' { Write-FakeFile (Join-Path $state.Root 'Cargo.lock') 'changed' }
            'add' { Write-FakeFile (Join-Path $state.Root 'crates\cortex-local\src\new.rs') 'new' }
            # 不执行删除；把已记录输入移到假源码包外，模拟当前清单缺项。
            'delete' { Move-Item -LiteralPath (Join-Path $state.Root 'crates\helper\src\lib.rs') -Destination (Join-Path $state.CaseRoot 'retained-lib.rs') }
            'config' { Write-FakeFile (Join-Path $state.Root '.cargo\config.toml') 'changed' }
            'migration' { Write-FakeFile (Join-Path $state.Root 'migrations\001.sql') 'changed' }
            'version' { Write-FakeFile (Join-Path $state.Root 'Cargo.toml') "[workspace.package]`nversion = `"9.8.8`"`n" }
        }
        Assert-Rejected { Assert-CortexTestArtifactProof $state.Root $state.Exe $build.testArtifactProofPath } '输入发生变化|版本不一致'
    }
}

Test-Case '拒绝编译过程中的源码变化且不生成 proof' {
    $state = New-FakeWorkspace
    $state.Mutation = { param($current) Write-FakeFile $current.Artifact.target.src_path 'mid-build mutation' }
    Assert-Rejected { New-FakeProof $state } '编译前后'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $state.CaseRoot 'proof\test-artifact-proof.json'))) '输入变化时不得产出 proof。'
}

Test-Case 'Cargo 非零退出不能用旧 exe 生成 proof' {
    $state = New-FakeWorkspace
    $state.BuildExitCode = 101
    Assert-Rejected { New-FakeProof $state } 'exit=101'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $state.CaseRoot 'proof\test-artifact-proof.json'))) 'Cargo 失败必须没有 proof。'
    Assert-True (Test-Path -LiteralPath (Join-Path $state.CaseRoot 'proof\inputs-before.json')) 'Cargo 失败仍须保留编译前输入记录。'
    Assert-True (Test-Path -LiteralPath (Join-Path $state.CaseRoot 'proof\inputs-after.json')) 'Cargo 失败仍须保留编译后输入记录。'
}

foreach ($mutationName in @('exe', 'ndjson', 'snapshot', 'target', 'profile', 'arguments', 'artifact', 'exePath')) {
    Test-Case "拒绝来源记录不匹配：$mutationName" {
        $state = New-FakeWorkspace
        $build = New-FakeProof $state
        $proof = ConvertFrom-Json ([IO.File]::ReadAllText($build.testArtifactProofPath))
        $exePath = $state.Exe
        switch ($mutationName) {
            'exe' { Write-FakeFile $state.Exe 'tampered executable' }
            'ndjson' { Write-FakeFile (Join-Path $state.CaseRoot 'proof\cargo-messages.ndjson') '{}' }
            'snapshot' { $proof.inputsBefore.sha256 = 'bad' }
            'target' { $proof.target = 'aarch64-pc-windows-msvc' }
            'profile' { $proof.profile = 'debug' }
            'arguments' { $proof.cargoArguments = @('test', '--no-run') }
            'artifact' { $proof.artifact.fresh = -not $proof.artifact.fresh }
            'exePath' { $exePath = Join-Path $state.CaseRoot 'different.exe'; Write-FakeFile $exePath }
        }
        Save-FakeProof $build.testArtifactProofPath $proof
        Assert-Rejected { Assert-CortexTestArtifactProof $state.Root $exePath $build.testArtifactProofPath } '不匹配|构建契约'
    }
}

foreach ($mutationName in @('lib', 'integration', 'name', 'package', 'notTest', 'manifest', 'source',
    'wrongTriple', 'debug', 'missingExe', 'filenames', 'duplicate', 'truncated', 'failed', 'unfinished', 'afterFinished')) {
    Test-Case "拒绝不合格 NDJSON：$mutationName" {
        $state = New-FakeWorkspace
        $context = Get-CortexArtifactContext $state.Root
        $artifact = ConvertFrom-Json (ConvertTo-Json -InputObject $state.Artifact -Depth 20)
        switch ($mutationName) {
            'lib' { $artifact.target.kind = @('lib') }
            'integration' { $artifact.target.kind = @('test') }
            'name' { $artifact.target.name = 'other' }
            'package' { $artifact.package_id = 'other' }
            'notTest' { $artifact.profile.test = $false }
            'manifest' { $artifact.manifest_path = Join-Path $state.Root 'Cargo.toml' }
            'source' { $artifact.target.src_path = Join-Path $state.Root 'crates\cortex-local\src\lib.rs' }
            'wrongTriple' { $artifact.executable = $artifact.executable.Replace('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc') }
            'debug' { $artifact.executable = $artifact.executable.Replace('\release\', '\debug\') }
            'missingExe' { $artifact.executable = $null }
            'filenames' { $artifact.filenames = @() }
        }
        $line = ConvertTo-Json -InputObject $artifact -Depth 20 -Compress
        $text = $line + "`n" + '{"reason":"build-finished","success":true}' + "`n"
        switch ($mutationName) {
            'duplicate' { $text = $line + "`n" + $text }
            'truncated' { $text = '{"reason":' }
            'failed' { $text = $line + "`n" + '{"reason":"build-finished","success":false}' }
            'unfinished' { $text = $line }
            'afterFinished' { $text += $line }
        }
        $log = Join-Path $state.CaseRoot 'invalid.ndjson'
        Write-FakeFile $log $text
        Assert-Rejected { Read-CortexTestCompilerArtifact $log $context } 'compiler-artifact|executable|NDJSON|build-finished|构建|test executable'
    }
}

foreach ($directoryKind in @('driveRoot', 'workspaceRoot', 'source', 'distRoot', 'outside', 'parentTraversal', 'inputOverlap', 'existing')) {
    Test-Case "拒绝危险输出且不删除：$directoryKind" {
        $state = New-FakeWorkspace
        $output = Join-Path $state.Root 'dist\safe'
        $inputs = @($state.Exe)
        switch ($directoryKind) {
            'driveRoot' { $output = [IO.Path]::GetPathRoot($state.Root) }
            'workspaceRoot' { $output = $state.Root }
            'source' { $output = Join-Path $state.Root 'crates\cortex-local' }
            'distRoot' { $output = Join-Path $state.Root 'dist' }
            'outside' { $output = Join-Path $state.CaseRoot 'elsewhere' }
            'parentTraversal' { $output = Join-Path $state.Root 'dist\..\crates' }
            'inputOverlap' { $inputs = @(Join-Path $output 'keep.exe'); Write-FakeFile $inputs[0] 'canary' }
            'existing' { Write-FakeFile (Join-Path $output 'canary.txt') 'canary' }
        }
        Assert-Rejected { Assert-CortexBundleOutput $state.Root $output $inputs } 'OutputDirectory|输出'
        Assert-True (Test-Path -LiteralPath $state.Exe) '危险输出校验不得删除输入。'
        if ($directoryKind -eq 'existing') {
            Assert-True ([IO.File]::ReadAllText((Join-Path $output 'canary.txt')) -ceq 'canary') '已有输出不得删除。'
        }
    }
}

Test-Case 'builder 的 -Force 保留已有目录和 sentinel' {
    $state = New-FakeWorkspace
    $build = New-FakeProof $state
    $arguments = Get-FakeBundleArguments $state $build
    $sentinel = Join-Path $arguments.OutputDirectory 'sentinel.txt'
    Write-FakeFile $sentinel 'keep'
    $arguments.Force = $true
    Assert-Rejected { & (Join-Path $state.FixtureRoot 'build-second-host-bundle.ps1') @arguments } '不再递归覆盖'
    Assert-True ([IO.File]::ReadAllText($sentinel) -ceq 'keep') '-Force 不得删除 sentinel。'
}

foreach ($suffix in @('.zip', '.zip.sha256')) {
    Test-Case "已有输出附件不覆盖：$suffix" {
        $state = New-FakeWorkspace
        $output = Join-Path $state.Root 'dist\safe'
        Write-FakeFile ($output + $suffix) 'keep'
        Assert-Rejected { Assert-CortexBundleOutput $state.Root $output @($state.Exe) } '输出已存在'
        Assert-True ([IO.File]::ReadAllText($output + $suffix) -ceq 'keep') '已有 zip/checksum 不得覆盖。'
    }
}

Test-Case '拒绝经过目录 junction 的输出' {
    $state = New-FakeWorkspace
    $outside = Join-Path $state.CaseRoot 'junction-target'
    $null = [IO.Directory]::CreateDirectory($outside)
    $null = New-Item -ItemType Junction -Path (Join-Path $state.Root 'dist') -Target $outside
    Assert-Rejected { Assert-CortexBundleOutput $state.Root (Join-Path $state.Root 'dist\unsafe') @($state.Exe) } 'reparse point'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $outside 'unsafe'))) 'junction 目标不得被写入。'
}

Test-Case 'producer 不覆盖旧 proof 或写入源码目录' {
    $state = New-FakeWorkspace
    $build = New-FakeProof $state
    $oldHash = Get-CortexArtifactSha256 $build.testArtifactProofPath
    Assert-Rejected { New-FakeProof $state } '全新目录'
    Assert-Rejected { New-CortexTestArtifactProof $state.Root (Join-Path $state.Root 'crates\new-output') } '源码目录'
    Assert-True ((Get-CortexArtifactSha256 $build.testArtifactProofPath) -ceq $oldHash) '原 proof 不得被覆盖。'
}

# 保留临时证据，不运行递归清理，也不接触真实 workspace 的构建目录。
Write-Output "PASS total=$script:passed; fake-file evidence=$script:testRoot"
}
finally {
    # 直接在调用者 PowerShell 中运行也不能遗留 cargo 替身，影响之后的真实构建。
    $env:CARGO_HOME = $script:previousCargoHome
    if ($null -ne $script:previousCargoFunction) {
        Set-Item -LiteralPath Function:global:cargo -Value $script:previousCargoFunction.ScriptBlock
    }
    else { Remove-Item -LiteralPath Function:global:cargo }
    if ($null -ne $script:previousState) { $global:CortexArtifactRegressionState = $script:previousState.Value }
    else { Remove-Variable -Name CortexArtifactRegressionState -Scope Global -ErrorAction SilentlyContinue }
}
