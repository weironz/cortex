param(
    [string]$TestExePath = "",
    [string]$TestArtifactProofPath = "",
    [string]$FixtureExePath = "",
    [string]$WindowsFixtureExePath = "",
    [string]$VCRuntimePath = "",
    [string]$CuaDirectory = "",
    [string]$InstallerPath = "",
    [string]$OutputDirectory = "",
    [switch]$Force
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$fixtureRoot = $PSScriptRoot
. ([IO.Path]::Combine($fixtureRoot, 'test-artifact-provenance.ps1'))
$repoRoot = [IO.Path]::GetFullPath((Join-Path $fixtureRoot "..\..\.."))
# 在创建/复制输出前，先证明这是当前源码的本次 Cargo 产物。
$testProof = Assert-CortexTestArtifactProof -RepositoryRoot $repoRoot `
    -TestExePath $TestExePath -ProofPath $TestArtifactProofPath
$version = $testProof.context.version
$testNdjsonPath = Join-Path ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($TestArtifactProofPath))) `
    'cargo-messages.ndjson'

$stage = Join-Path $repoRoot (
    "dist\cortex-desktop-v{0}-x86_64-pc-windows-msvc" -f $version
)
$setupPath = "$stage-setup.exe"
if ([string]::IsNullOrWhiteSpace($FixtureExePath)) {
    $FixtureExePath = Join-Path $fixtureRoot `
        "target\cortex-computer-use-soak-fixture.exe"
}
if ([string]::IsNullOrWhiteSpace($WindowsFixtureExePath)) {
    $WindowsFixtureExePath = Join-Path $fixtureRoot `
        "target\cortex-computer-use-windows-fixture.exe"
}
if ([string]::IsNullOrWhiteSpace($VCRuntimePath)) {
    $VCRuntimePath = Join-Path $stage "vcruntime140.dll"
}
if ([string]::IsNullOrWhiteSpace($CuaDirectory)) {
    $CuaDirectory = Join-Path $stage "cua"
}
if ([string]::IsNullOrWhiteSpace($InstallerPath)) {
    $InstallerPath = $setupPath
}
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot (
        "dist\cortex-computer-use-second-host-v{0}" -f $version
    )
}

$InstallerSbomPath = "$InstallerPath.cdx.json"
foreach ($file in @($TestExePath, $FixtureExePath, $WindowsFixtureExePath, $VCRuntimePath, $InstallerPath, $InstallerSbomPath)) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        throw "Required file is missing: $file"
    }
}
if (-not (Test-Path -LiteralPath $CuaDirectory -PathType Container)) {
    throw "Required Cua directory is missing: $CuaDirectory"
}

$cuaFiles = @(
    "cua-driver.exe", "cua-driver-uia.exe", "cua-cursor-theme.exe", "cua_driver_sdk.dll",
    "cua_driver_node_runtime.node", "cua_driver_abi.h", "LICENSE.md", "THIRD_PARTY_NOTICES.md",
    "SBOM.spdx.json", "README.md"
)
$fixtureFiles = @(
    'acceptance-report.ps1', 'portable-second-host-smoke.ps1', 'run-second-host-acceptance.ps1',
    'interactive-desktop.ps1', 'interactive-desktop-selftest.ps1',
    'e2e.ps1', 'cleanup-artifacts.ps1',
    'chrome\acceptance.html', 'chrome\page2.html'
)
$inputPaths = @($TestExePath, $TestArtifactProofPath, $testNdjsonPath, $FixtureExePath, $WindowsFixtureExePath,
    $VCRuntimePath, $InstallerPath, $InstallerSbomPath, $CuaDirectory)
$inputPaths += @($fixtureFiles | ForEach-Object { Join-Path $fixtureRoot $_ })
$inputPaths += @($cuaFiles | ForEach-Object { Join-Path $CuaDirectory $_ })
foreach ($path in $inputPaths) {
    if (-not (Test-Path -LiteralPath $path)) { throw "打包输入缺失：$path" }
    Assert-CortexArtifactNoReparse $path
}
$OutputDirectory = Assert-CortexBundleOutput -RepositoryRoot $repoRoot `
    -OutputDirectory $OutputDirectory -InputPaths $inputPaths
$zipPath = "$OutputDirectory.zip"
$null = New-Item -ItemType Directory -Path $OutputDirectory

Copy-Item -LiteralPath (Join-Path $fixtureRoot 'acceptance-report.ps1') `
    -Destination (Join-Path $OutputDirectory 'acceptance-report.ps1')

Copy-Item -LiteralPath $TestExePath `
    -Destination (Join-Path $OutputDirectory "cortex-local-tests.exe")
Copy-Item -LiteralPath $TestArtifactProofPath `
    -Destination (Join-Path $OutputDirectory 'test-artifact-proof.json')
Copy-Item -LiteralPath $testNdjsonPath `
    -Destination (Join-Path $OutputDirectory 'cargo-messages.ndjson')
# 桌面模块的接口由主线/runner 维护；这里仅复制明确要求的两个文件。
foreach ($name in @('interactive-desktop.ps1', 'interactive-desktop-selftest.ps1')) {
    Copy-Item -LiteralPath (Join-Path $fixtureRoot $name) -Destination (Join-Path $OutputDirectory $name)
}
Copy-Item -LiteralPath $FixtureExePath `
    -Destination (Join-Path $OutputDirectory "cortex-computer-use-soak-fixture.exe")
Copy-Item -LiteralPath $VCRuntimePath `
    -Destination (Join-Path $OutputDirectory "vcruntime140.dll")
Copy-Item -LiteralPath (Join-Path $fixtureRoot "portable-second-host-smoke.ps1") `
    -Destination (Join-Path $OutputDirectory "portable-second-host-smoke.ps1")
Copy-Item -LiteralPath (Join-Path $fixtureRoot "run-second-host-acceptance.ps1") `
    -Destination (Join-Path $OutputDirectory "run-second-host-acceptance.ps1")
Copy-Item -LiteralPath (Join-Path $fixtureRoot "e2e.ps1") `
    -Destination (Join-Path $OutputDirectory "e2e.ps1")
Copy-Item -LiteralPath (Join-Path $fixtureRoot "cleanup-artifacts.ps1") `
    -Destination (Join-Path $OutputDirectory "cleanup-artifacts.ps1")

$targetOutput = Join-Path $OutputDirectory "target"
$chromeOutput = Join-Path $OutputDirectory "chrome"
$installerOutput = Join-Path $OutputDirectory "installer"
$null = New-Item -ItemType Directory -Path $targetOutput
$null = New-Item -ItemType Directory -Path $chromeOutput
$null = New-Item -ItemType Directory -Path $installerOutput
Copy-Item -LiteralPath $WindowsFixtureExePath `
    -Destination (Join-Path $targetOutput "cortex-computer-use-windows-fixture.exe")
foreach ($name in @("acceptance.html", "page2.html")) {
    Copy-Item -LiteralPath (Join-Path $fixtureRoot "chrome\$name") `
        -Destination (Join-Path $chromeOutput $name)
}
Copy-Item -LiteralPath $InstallerPath `
    -Destination (Join-Path $installerOutput ([IO.Path]::GetFileName($InstallerPath)))
Copy-Item -LiteralPath $InstallerSbomPath `
    -Destination (Join-Path $installerOutput ([IO.Path]::GetFileName($InstallerSbomPath)))
$installer = Join-Path $installerOutput ([IO.Path]::GetFileName($InstallerPath))
$installerSbom = Join-Path $installerOutput ([IO.Path]::GetFileName($InstallerSbomPath))

$cuaOutput = Join-Path $OutputDirectory "cua"
$null = New-Item -ItemType Directory -Path $cuaOutput
foreach ($name in $cuaFiles) {
    $source = Join-Path $CuaDirectory $name
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Cua file is missing: $source"
    }
    Copy-Item -LiteralPath $source -Destination (Join-Path $cuaOutput $name)
}

function Get-Sha256 {
    param([string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

$hashes = [ordered]@{
    testExe = Get-Sha256 (Join-Path $OutputDirectory "cortex-local-tests.exe")
    testArtifactProof = Get-Sha256 (Join-Path $OutputDirectory 'test-artifact-proof.json')
    testArtifactNdjson = Get-Sha256 (Join-Path $OutputDirectory 'cargo-messages.ndjson')
    fixtureExe = Get-Sha256 (
        Join-Path $OutputDirectory "cortex-computer-use-soak-fixture.exe"
    )
    windowsFixtureExe = Get-Sha256 (
        Join-Path $targetOutput "cortex-computer-use-windows-fixture.exe"
    )
    vcRuntime = Get-Sha256 (Join-Path $OutputDirectory "vcruntime140.dll")
    installer = Get-Sha256 $installer
    installerSbom = Get-Sha256 $installerSbom
    acceptanceRunner = Get-Sha256 (Join-Path $OutputDirectory "run-second-host-acceptance.ps1")
    acceptanceReport = Get-Sha256 (Join-Path $OutputDirectory 'acceptance-report.ps1')
    interactiveDesktop = Get-Sha256 (Join-Path $OutputDirectory 'interactive-desktop.ps1')
    interactiveDesktopSelftest = Get-Sha256 (Join-Path $OutputDirectory 'interactive-desktop-selftest.ps1')
    portableRunner = Get-Sha256 (Join-Path $OutputDirectory "portable-second-host-smoke.ps1")
    e2eHarness = Get-Sha256 (Join-Path $OutputDirectory "e2e.ps1")
    cua = [ordered]@{}
}
if ($hashes.testExe -cne $testProof.testExeSha256 -or $hashes.testArtifactNdjson -cne $testProof.ndjsonSha256 -or
    $hashes.testArtifactProof -cne (Get-Sha256 $TestArtifactProofPath)) {
    throw '复制期间测试 exe/proof/NDJSON 变化；拒绝生成 bundle。'
}
# 再查一次当前输入，防止打包复制期间有人继续改 Rust/Cargo 源码。
$null = Assert-CortexTestArtifactProof -RepositoryRoot $repoRoot `
    -TestExePath $TestExePath -ProofPath $TestArtifactProofPath
foreach ($name in $cuaFiles) {
    $hashes.cua[$name] = Get-Sha256 (Join-Path $cuaOutput $name)
}
$manifest = [ordered]@{
    schemaVersion = 1
    product = "Cortex Computer Use portable second-host acceptance"
    version = $version
    generatedAt = [DateTimeOffset]::UtcNow.ToString("o")
    source = [ordered]@{
        testExe = [IO.Path]::GetFullPath($TestExePath)
        testArtifactProof = [IO.Path]::GetFullPath($TestArtifactProofPath)
        fixtureExe = [IO.Path]::GetFullPath($FixtureExePath)
        windowsFixtureExe = [IO.Path]::GetFullPath($WindowsFixtureExePath)
        vcRuntime = [IO.Path]::GetFullPath($VCRuntimePath)
        cuaDirectory = [IO.Path]::GetFullPath($CuaDirectory)
    }
    sha256 = $hashes
    # 现有 runner 使用 sha256；hashes 中按主线约定同时暴露桌面模块的两个条目。
    hashes = [ordered]@{
        interactiveDesktop = $hashes.interactiveDesktop
        interactiveDesktopSelftest = $hashes.interactiveDesktopSelftest
    }
    testArtifact = [ordered]@{
        target = $testProof.target
        profile = $testProof.profile
        sourceInputsSha256 = $testProof.inputsAfter.sha256
        cargoFresh = $testProof.artifact.fresh
        scope = 'Local build-source association only; no signature or cross-host source validation.'
    }
}
$manifestPath = Join-Path $OutputDirectory "SHA256SUMS.json"
[IO.File]::WriteAllText(
    $manifestPath,
    ($manifest | ConvertTo-Json -Depth 8) + [Environment]::NewLine,
    [Text.UTF8Encoding]::new($false)
)

$runText = @(
    "Cortex Computer Use second-host acceptance bundle",
    "",
    "Run this from an unlocked interactive Windows x64 desktop:",
    "",
    "powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\run-second-host-acceptance.ps1",
    "",
    "The wrapper validates all hashes, runs the portable secure/soak test,",
    "then runs the 20-task E2E harness and writes .\results\run-<id>\summary.json.",
    "The newest run is referenced by .\results\latest-run.json; failed runs are retained.",
    "",
    "To run the portable secure/soak test manually:",
    "",
    'powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\portable-second-host-smoke.ps1 `',
    '  -TestExePath (Resolve-Path .\cortex-local-tests.exe).Path `',
    '  -FixtureExePath (Resolve-Path .\cortex-computer-use-soak-fixture.exe).Path `',
    '  -VCRuntimePath (Resolve-Path .\vcruntime140.dll).Path `',
    '  -CuaDirectory (Resolve-Path .\cua).Path `',
    '  -ExpectedTestSha256 "<sha256.testExe from SHA256SUMS.json>" `',
    '  -ExpectedFixtureSha256 "<sha256.fixtureExe from SHA256SUMS.json>" `',
    '  -ExpectedVCRuntimeSha256 "<sha256.vcRuntime from SHA256SUMS.json>" `',
    '  -OutputPath "$env:TEMP\cortex-second-host-result.json"',
    "",
    "First use -ValidateOnly on the same command. That checks paths and hashes only.",
    "Do not use an SSH/service session as a substitute for an unlocked desktop.",
    "A result proves this binary and this host run only; it is not a source or GA claim.",
    "test-artifact-proof.json and cargo-messages.ndjson record the build host source association.",
    "This record is not a signature and does not verify source freshness on another host.",
    "",
    "The unsigned desktop installer is under .\installer.",
    "The unified CycloneDX SBOM is next to it as setup.exe.cdx.json.",
    "For the 20-task UI harness, run e2e.ps1 with -NoBuild and an OutputPath.",
    "The harness reads its fixture from .\target and its pages from .\chrome.",
    "This installer is not acceptable as a signed production release.",
    ""
) -join [Environment]::NewLine
[IO.File]::WriteAllText(
    (Join-Path $OutputDirectory "RUN.txt"),
    $runText,
    [Text.UTF8Encoding]::new($false)
)

Compress-Archive -LiteralPath $OutputDirectory -DestinationPath $zipPath `
    -CompressionLevel Optimal
$zipSha256 = Get-Sha256 $zipPath
[IO.File]::WriteAllText(
    "$zipPath.sha256",
    "$zipSha256 *$([IO.Path]::GetFileName($zipPath))" + [Environment]::NewLine,
    [Text.UTF8Encoding]::new($false)
)

[pscustomobject]@{
    version = $version
    bundleDirectory = $OutputDirectory
    bundleZip = $zipPath
    bundleZipSha256 = $zipSha256
    testExeSha256 = $hashes.testExe
    fixtureExeSha256 = $hashes.fixtureExe
    vcRuntimeSha256 = $hashes.vcRuntime
    installerSha256 = $hashes.installer
    installerSbomSha256 = $hashes.installerSbom
} | Format-List
