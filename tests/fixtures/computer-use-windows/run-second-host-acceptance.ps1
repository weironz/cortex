param(
    [string]$ResultDirectory = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'acceptance-report.ps1')
foreach ($module in @(
    "Microsoft.PowerShell.Utility",
    "Microsoft.PowerShell.Management",
    "Microsoft.PowerShell.Security"
)) {
    $modulePath = [IO.Path]::Combine($PSHOME, "Modules", $module, "$module.psd1")
    Import-Module -Name $modulePath -ErrorAction Stop
}

$bundle = $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ResultDirectory)) {
    $ResultDirectory = Join-Path $bundle "results"
}
$null = New-Item -ItemType Directory -Path $ResultDirectory -Force
$resultRoot = (Resolve-Path -LiteralPath $ResultDirectory).ProviderPath
# portable runner 拒绝覆盖结果；每轮使用独立目录，失败也不得遗留本轮旧 PASS。
$ResultDirectory = Join-Path $resultRoot ('run-' + [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $ResultDirectory
$summaryPath = Join-Path $ResultDirectory 'summary.json'
@{ result = 'running'; generatedAt = [DateTime]::UtcNow.ToString('o') } |
    ConvertTo-Json | Set-Content -LiteralPath $summaryPath -Encoding UTF8
@{ resultDirectory = $ResultDirectory; summaryPath = $summaryPath } |
    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $resultRoot 'latest-run.json') -Encoding UTF8
try {
$manifest = Get-Content -LiteralPath (Join-Path $bundle "SHA256SUMS.json") -Raw -Encoding UTF8 |
    ConvertFrom-Json

if (-not [Environment]::Is64BitOperatingSystem) {
    throw "This acceptance bundle requires Windows x64."
}

. (Join-Path $bundle 'interactive-desktop.ps1')

function Invoke-ChildPowerShell {
    param(
        [string]$Script,
        [string[]]$Arguments
    )
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $Script @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$([IO.Path]::GetFileName($Script)) exited with code $LASTEXITCODE."
    }
}

Assert-InteractiveDesktop

$portableScript = Join-Path $bundle "portable-second-host-smoke.ps1"
$testExe = Join-Path $bundle "cortex-local-tests.exe"
$fixtureExe = Join-Path $bundle "cortex-computer-use-soak-fixture.exe"
$vcRuntime = Join-Path $bundle "vcruntime140.dll"
$cuaDirectory = Join-Path $bundle "cua"
$e2eScript = Join-Path $bundle "e2e.ps1"
$portableOutput = Join-Path $ResultDirectory "portable-smoke.json"
$e2eOutput = Join-Path $ResultDirectory "e2e.json"

$installerDirectory = Join-Path $bundle "installer"
$installers = @(
    Get-ChildItem -LiteralPath $installerDirectory -Filter "*-setup.exe" -File
)
if ($installers.Count -ne 1) {
    throw "Expected exactly one desktop installer in $installerDirectory."
}
$installer = $installers[0]
$installerSbom = "$($installer.FullName).cdx.json"
if (-not (Test-Path -LiteralPath $installerSbom -PathType Leaf)) {
    throw "The installer SBOM is missing: $installerSbom"
}
$installerHash = (
    Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256
).Hash.ToLowerInvariant()
$installerSbomHash = (
    Get-FileHash -LiteralPath $installerSbom -Algorithm SHA256
).Hash.ToLowerInvariant()
if ($installerHash -ne [string]$manifest.sha256.installer) {
    throw "The installer SHA-256 does not match SHA256SUMS.json."
}
if ($installerSbomHash -ne [string]$manifest.sha256.installerSbom) {
    throw "The installer SBOM SHA-256 does not match SHA256SUMS.json."
}
$bom = Get-Content -LiteralPath $installerSbom -Raw -Encoding UTF8 | ConvertFrom-Json
if ($bom.bomFormat -ne "CycloneDX" -or $bom.specVersion -ne "1.5") {
    throw "The installer SBOM is not CycloneDX 1.5."
}
$installerComponent = $bom.components |
    Where-Object {
        $_.type -eq "file" -and $_.name -eq $installer.Name
    } |
    Select-Object -First 1
if (
    $null -eq $installerComponent -or
    $installerComponent.hashes[0].content -ne $installerHash
) {
    throw "The SBOM does not contain the verified installer hash."
}

$portableArguments = @(
    "-TestExePath", $testExe,
    "-FixtureExePath", $fixtureExe,
    "-VCRuntimePath", $vcRuntime,
    "-CuaDirectory", $cuaDirectory,
    "-ExpectedTestSha256", [string]$manifest.sha256.testExe,
    "-ExpectedFixtureSha256", [string]$manifest.sha256.fixtureExe,
    "-ExpectedVCRuntimeSha256", [string]$manifest.sha256.vcRuntime,
    "-OutputPath", $portableOutput
)
Invoke-ChildPowerShell -Script $portableScript `
    -Arguments ($portableArguments + @("-ValidateOnly"))
Invoke-ChildPowerShell -Script $portableScript `
    -Arguments $portableArguments

Invoke-ChildPowerShell -Script $e2eScript -Arguments @(
    "-NoBuild",
    "-OutputPath", $e2eOutput
)

$portable = Get-Content -LiteralPath $portableOutput -Raw -Encoding UTF8 | ConvertFrom-Json
$e2e = Read-E2EReport -Path $e2eOutput
Assert-E2EReport -Report $e2e

$summary = [ordered]@{
    schemaVersion = 1
    suite = "cortex-computer-use-second-host"
    generatedAt = [DateTime]::UtcNow.ToString("o")
    result = "pass"
    host = $env:COMPUTERNAME
    os = [Environment]::OSVersion.VersionString
    installer = [ordered]@{
        file = $installer.Name
        sha256 = $installerHash
        sbomSha256 = $installerSbomHash
        bomFormat = $bom.bomFormat
        specVersion = $bom.specVersion
        authenticode = (
            Get-AuthenticodeSignature -LiteralPath $installer.FullName
        ).Status.ToString()
    }
    portableResult = $portable
    e2eSummary = [ordered]@{
        total = $e2e.summary.total
        pass = $e2e.summary.pass
        fail = $e2e.summary.fail
        skipped = $e2e.summary.skipped
        scope = $e2e.scope
        desktopLockedAtReport = $e2e.host.desktopLockedAtReport
    }
}
$summaryPath = Join-Path $ResultDirectory "summary.json"
[IO.File]::WriteAllText(
    $summaryPath,
    ($summary | ConvertTo-Json -Depth 10) + [Environment]::NewLine,
    [Text.UTF8Encoding]::new($false)
)
Write-Output "Second-host acceptance passed: $summaryPath"
}
catch {
    @{ result = 'failed'; generatedAt = [DateTime]::UtcNow.ToString('o'); failure = $_.Exception.Message } |
        ConvertTo-Json | Set-Content -LiteralPath $summaryPath -Encoding UTF8
    throw
}
