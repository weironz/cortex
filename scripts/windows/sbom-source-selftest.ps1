#requires -Version 5.1
<#
只验证 Cargo 来源 metadata 和完整 SBOM 的源码 fingerprint 范围。
临时 workspace 无远程依赖；不编译，不执行 stage 中的占位 exe，不证明构建、签名或桌面验收。
完整生产生成器读取临时 Git/Cargo/app 源码夹具，验证 tracked/untracked 的非 Cua C 文件变化。
app 的 pubspec/package_config、SDK 版本和 CRT 都是明确的 metadata fixture；不需要 flutter pub get。
主 SourceRoot 只提供生产脚本和真实 Cua SPDX；不读写主 app/.dart_tool，不要求 Flutter SDK 或依赖缓存。
输出只写入 C:\Temp 的独占目录；清理只删除已登记的具体文件和空目录，遇到额外文件就失败。

运行：powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/windows/sbom-source-selftest.ps1
#>
[CmdletBinding()]
param(
    [string]$SourceRoot = '',
    [string]$TemporaryRoot = 'C:\Temp'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
foreach ($module in @('Microsoft.PowerShell.Utility', 'Microsoft.PowerShell.Management')) {
    # Git Bash 可能继承另一版 PowerShell 的模块路径；不依赖模块自动发现。
    Import-Module ([IO.Path]::Combine($PSHOME, 'Modules', $module, "$module.psd1")) -ErrorAction Stop
}
. (Join-Path $PSScriptRoot 'native-json.ps1')
. (Join-Path $PSScriptRoot 'sbom-cargo-source.ps1')

if ([string]::IsNullOrWhiteSpace($SourceRoot)) {
    # PS 5.1 在参数默认值求值时尚未提供 PSScriptRoot。
    $SourceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
}
$source = (Resolve-Path -LiteralPath $SourceRoot).ProviderPath.TrimEnd('\', '/')
$tempBase = [IO.Path]::GetFullPath($TemporaryRoot).TrimEnd('\', '/')
if ([IO.Path]::GetPathRoot($tempBase) -ine 'C:\') {
    throw 'Self-test outputs must stay on the C: temporary volume.'
}
$token = [Guid]::NewGuid().ToString('N')
$testRoot = Join-Path $tempBase ('cortex-sbom-source-selftest-' + $token)
$fingerprintRoot = Join-Path $testRoot 'fingerprint-repo'
$thirdParty = Join-Path $fingerprintRoot 'third_party'
$probeDirectory = Join-Path $thirdParty ('sbom-source-regression-' + $token)
$probePath = Join-Path $probeDirectory 'fingerprint.c'
$probeRelative = 'third_party/sbom-source-regression-' + $token + '/fingerprint.c'
$generator = Join-Path $source 'scripts/generate-windows-sbom.ps1'
$fixtureRoot = Join-Path $testRoot 'repo'
$fixtureManifest = Join-Path $fixtureRoot 'Cargo.toml'
$vendorManifest = Join-Path $fixtureRoot 'third_party/vendor/Cargo.toml'
$siblingRoot = Join-Path $testRoot 'repo-sibling'
$stage = Join-Path $testRoot 'stage'
$targetDirectory = Join-Path $testRoot 'cargo-target'
$flutterFixture = Join-Path $testRoot 'flutter-metadata-fixture'
$emptyGitTemplate = Join-Path $testRoot 'empty-git-template'
$emptyGitConfig = Join-Path $testRoot 'empty-git-config'
$ownedFiles = New-Object 'System.Collections.Generic.List[string]'
$ownedDirectories = New-Object 'System.Collections.Generic.List[string]'
$script:checks = 0
$script:metadataRuns = 0
$script:rejectedMetadataCases = 0
$script:generatorRuns = 0
$createdTempBase = $false
$savedEnvironment = [ordered]@{}
foreach ($name in @('CARGO_NET_OFFLINE', 'CARGO_TARGET_DIR', 'FLUTTER_ROOT',
    'GIT_CONFIG_GLOBAL', 'GIT_CONFIG_NOSYSTEM', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG_COUNT',
    'GIT_DIR', 'GIT_COMMON_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_OBJECT_DIRECTORY',
    'GIT_ALTERNATE_OBJECT_DIRECTORIES')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

function Assert-Check {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw "SBOM source self-test failed: $Message" }
    $script:checks++
}

function Assert-SafePath {
    param([string]$Path, [string]$Boundary, [switch]$AllowBoundary)
    $full = [IO.Path]::GetFullPath($Path).TrimEnd('\', '/')
    $bound = [IO.Path]::GetFullPath($Boundary).TrimEnd('\', '/')
    $inside = $full.StartsWith($bound + '\', [StringComparison]::OrdinalIgnoreCase)
    if (-not $inside -and -not ($AllowBoundary -and $full -ieq $bound)) {
        throw "Refusing an operation outside its absolute path boundary: $full"
    }
    # 词法边界不足以保护 junction；检查现存路径及祖先，拒绝重解析点。
    $cursor = $full
    while ($cursor.Length -ge $bound.Length) {
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Refusing an operation through a reparse point: $cursor"
            }
        }
        if ($cursor -ieq $bound) { break }
        $parent = [IO.Path]::GetDirectoryName($cursor)
        if ([string]::IsNullOrEmpty($parent) -or $parent -eq $cursor) { break }
        $cursor = $parent.TrimEnd('\', '/')
    }
}

function Add-OwnedDirectory {
    param([string]$Path, [string]$Boundary)
    Assert-SafePath -Path $Path -Boundary $Boundary
    if (Test-Path -LiteralPath $Path) { throw "Fixture directory already exists: $Path" }
    if (-not [IO.Directory]::Exists([IO.Path]::GetDirectoryName($Path))) {
        throw "Fixture parent must already exist: $Path"
    }
    $ownedDirectories.Add($Path)
    $null = [IO.Directory]::CreateDirectory($Path)
}

function Register-OwnedFile {
    param([string]$Path)
    Assert-SafePath -Path $Path -Boundary $testRoot
    if (-not $ownedFiles.Contains($Path)) {
        if (Test-Path -LiteralPath $Path) { throw "Fixture file already exists: $Path" }
        $ownedFiles.Add($Path)
    }
}

function Write-FixtureFile {
    param([string]$Path, [string]$Text)
    Register-OwnedFile $Path
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false))
}

function Get-FixtureMetadata {
    param([switch]$Initial)
    $arguments = @('metadata', '--manifest-path', $fixtureManifest, '--offline', '--format-version', '1')
    if (-not $Initial) { $arguments += '--locked' }
    Push-Location -LiteralPath $testRoot
    try { $result = Invoke-CortexNativeUtf8Json -FilePath 'cargo' -ArgumentList $arguments }
    finally { Pop-Location }
    $script:metadataRuns++
    return $result
}

function Get-FixturePackage {
    param($Metadata, [string]$Name)
    $matches = @($Metadata.packages | Where-Object { $_.name -ceq $Name })
    if ($matches.Count -ne 1) { throw "Expected exactly one real Cargo fixture package: $Name" }
    return $matches[0]
}

function Assert-SourceProperties {
    param([object[]]$Actual, [System.Collections.IDictionary]$Expected, [string]$Label)
    $valid = $Actual.Count -eq $Expected.Count
    $seen = @{}
    foreach ($property in $Actual) {
        if ($property -isnot [System.Collections.IDictionary] -or $property.Count -ne 2 -or
            -not $property.Contains('name') -or -not $property.Contains('value') -or
            $property.name -isnot [string] -or $property.value -isnot [string]) {
            $valid = $false
            continue
        }
        $name = [string]$property.name
        if ($seen.ContainsKey($name) -or -not $Expected.Contains($name) -or
            $property.value -cne $Expected[$name]) { $valid = $false }
        $seen[$name] = $true
    }
    Assert-Check $valid "$Label must return exactly the expected name/value property array."
}

function ConvertTo-VendorToml {
    param([hashtable]$Overrides = @{}, [string[]]$Omit = @())
    $fields = [ordered]@{
        upstream_crate_sha256 = '"' + ('a' * 64) + '"'
        native_backport_revision = '"' + ('b' * 40) + '"'
        native_backport_url = '"https://example.invalid/upstream/commit/backport"'
    }
    $lines = @('[package.metadata.cortex_vendor]')
    foreach ($name in $fields.Keys) {
        if ($Omit -ccontains $name) { continue }
        $value = if ($Overrides.ContainsKey($name)) { $Overrides[$name] } else { $fields[$name] }
        $lines += "$name = $value"
    }
    return ($lines -join "`n") + "`n"
}

function Get-BomProperty {
    param($Bom, [string]$Name)
    $matches = @($Bom.metadata.component.properties | Where-Object { $_.name -ceq $Name })
    if ($matches.Count -ne 1 -or $matches[0].value -isnot [string]) {
        throw "Expected exactly one SBOM metadata property: $Name"
    }
    return [string]$matches[0].value
}

function Get-FrozenInputState {
    # 这是独立夹具的内容对照清单，不计算或替代生产 fingerprint。
    $paths = @(Invoke-FixtureGit @('ls-files', '--cached', '--others', '--exclude-standard', '--',
        'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo', 'crates', 'app', 'scripts',
        'third_party', 'LICENSE', 'NOTICE', 'CHANGELOG.md'))
    $paths += 'app/.dart_tool/package_config.json'
    $lines = foreach ($relative in $paths | Sort-Object -Unique) {
        $normalized = $relative.Replace('\', '/')
        if ($normalized -ceq $probeRelative) { continue }
        $path = Join-Path $fingerprintRoot $relative
        $hash = if ([IO.File]::Exists($path)) {
            (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        } else { 'MISSING' }
        "$normalized $hash"
    }
    return $lines -join "`n"
}

function Invoke-FullSbom {
    param([string]$OutputPath)
    Register-OwnedFile $OutputPath
    # 不传 SourceFingerprint；调用完整生产脚本，而非复制 fingerprint 函数来断言自己。
    Push-Location -LiteralPath $testRoot
    try {
        & $generator -StageDirectory $stage -OutputPath $OutputPath -SourceRoot $fingerprintRoot `
            -Version '0.0.0-sbom-source-selftest' | Out-Null
    }
    finally { Pop-Location }
    $script:generatorRuns++
    return (Get-Content -LiteralPath $OutputPath -Raw -Encoding UTF8 | ConvertFrom-Json)
}

function Assert-NoBuildArtifacts {
    # metadata 会查询 rustc 版本并缓存此 JSON；允许它，不允许编译产物或额外目录。
    $entries = @(Get-ChildItem -LiteralPath $targetDirectory -Force)
    $unexpected = @($entries | Where-Object { $_.PSIsContainer -or $_.Name -cne '.rustc_info.json' })
    Assert-Check ($unexpected.Count -eq 0) 'The private target may contain only Cargo compiler-version metadata, never build artifacts.'
}

function Invoke-FixtureGit {
    param([string[]]$Arguments)
    # 空模板与禁用 hooks/signing 的私有 Git 仓库；不使用用户全局配置或主仓库索引。
    try {
        $result = @(& git -c "core.hooksPath=$emptyGitTemplate" -c core.autocrlf=false `
            -c commit.gpgSign=false -c tag.gpgSign=false -c gc.auto=0 `
            -c user.name=SbomFixture -c user.email=sbom-fixture@example.invalid `
            -C $fingerprintRoot @Arguments)
        if ($LASTEXITCODE -ne 0) { throw "Fixture git failed (exit $LASTEXITCODE): $($Arguments[0])" }
        return $result
    }
    finally {
        # Git 创建的索引、对象、refs 逐项登记；删除时仍逐文件验边界，不递归删除 .git。
        $gitDirectory = Join-Path $fingerprintRoot '.git'
        if ([IO.Directory]::Exists($gitDirectory)) {
            Assert-SafePath -Path $gitDirectory -Boundary $testRoot
            $items = @((Get-Item -LiteralPath $gitDirectory -Force)) +
                @(Get-ChildItem -LiteralPath $gitDirectory -Force -Recurse | Sort-Object FullName)
            foreach ($item in $items) {
                Assert-SafePath -Path $item.FullName -Boundary $testRoot
                if ($item.PSIsContainer) {
                    if (-not $ownedDirectories.Contains($item.FullName)) { $ownedDirectories.Add($item.FullName) }
                }
                elseif (-not $ownedFiles.Contains($item.FullName)) { $ownedFiles.Add($item.FullName) }
            }
        }
    }
}

function Assert-FingerprintPair {
    param($First, $Second, [string]$Label)
    foreach ($bom in @($First, $Second)) {
        Assert-Check ($bom.bomFormat -ceq 'CycloneDX' -and $bom.specVersion -ceq '1.5') `
            'A complete generator call must produce a CycloneDX 1.5 document.'
        Assert-Check ((Get-BomProperty $bom 'cortex:source-fingerprint-scope') -ceq
            'tracked-build-diff,untracked-build-inputs,lockfiles') 'The generator must calculate its own source fingerprint.'
        Assert-Check ((Get-BomProperty $bom 'cortex:source-fingerprint') -cmatch '^[a-f0-9]{64}$') `
            'The full SBOM must carry a SHA-256 source fingerprint.'
        $fileComponents = @($bom.components | Where-Object { $_.type -ceq 'file' })
        Assert-Check ($fileComponents.Count -eq 4 -and (Get-BomProperty $bom 'cortex:stage-files') -ceq '4') `
            'The full SBOM must include three text placeholders and the actual Cua SPDX.'
        foreach ($name in @('Flutter', 'Dart SDK', 'Cua Driver', 'Microsoft Visual C++ Redistributable Runtime')) {
            Assert-Check (@($bom.components | Where-Object { $_.name -ceq $name }).Count -eq 1) `
                "The full SBOM must contain exactly one $name component."
        }
        $cargo = @($bom.components | Where-Object { $_.name -ceq 'sbom-fingerprint-member' })
        Assert-Check ($cargo.Count -eq 1 -and (Get-BomProperty $bom 'cortex:cargo-packages') -ceq '1') `
            'The full generator must consume the real dependency-free Cargo fixture metadata.'
        $pub = @($bom.components | Where-Object { $_.name -ceq 'cortex_app' })
        Assert-Check ($pub.Count -eq 1 -and $pub[0].version -ceq '0.0.0-sbom-source-selftest' -and
            (Get-BomProperty $bom 'cortex:dart-packages') -ceq '1') 'The full generator must consume the private minimal app package_config.'
    }
    $before = Get-BomProperty $First 'cortex:source-fingerprint'
    $after = Get-BomProperty $Second 'cortex:source-fingerprint'
    Assert-Check ($before -cne $after) "The production fingerprint must detect $Label non-Cua C content changes."
    Assert-Check (($First.components | ConvertTo-Json -Depth 20 -Compress) -ceq
        ($Second.components | ConvertTo-Json -Depth 20 -Compress)) 'The component inventory must stay identical.'
    Assert-Check (($First.dependencies | ConvertTo-Json -Depth 20 -Compress) -ceq
        ($Second.dependencies | ConvertTo-Json -Depth 20 -Compress)) 'The dependency inventory must stay identical.'
    $stableBefore = @($First.metadata.component.properties | Where-Object { $_.name -cne 'cortex:source-fingerprint' })
    $stableAfter = @($Second.metadata.component.properties | Where-Object { $_.name -cne 'cortex:source-fingerprint' })
    Assert-Check (($stableBefore | ConvertTo-Json -Depth 5 -Compress) -ceq
        ($stableAfter | ConvertTo-Json -Depth 5 -Compress)) 'Other SBOM metadata, including Git commit/tag/dirty state, must stay identical within each pair.'
    Assert-Check ($First.serialNumber -cne $Second.serialNumber) 'The full SBOM identity must reflect the changed source fingerprint.'
    Write-Output "$Label full generator fingerprint before: $before"
    Write-Output "$Label full generator fingerprint after:  $after"
}

try {
    $productionInputs = @($generator, (Join-Path $source 'scripts/windows/native-json.ps1'),
        (Join-Path $source 'scripts/windows/sbom-cargo-source.ps1'),
        (Join-Path $source 'third_party/cua/SBOM.spdx.json'))
    foreach ($required in $productionInputs) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) { throw "Required input is missing: $required" }
    }
    $productionHashesBefore = @($productionInputs | ForEach-Object {
        (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash
    }) -join ','
    $null = Get-Command cargo, git -ErrorAction Stop
    if (-not (Test-Path -LiteralPath $tempBase)) {
        Assert-SafePath -Path $tempBase -Boundary ([IO.Path]::GetPathRoot($tempBase))
        if (-not [IO.Directory]::Exists([IO.Path]::GetDirectoryName($tempBase))) {
            throw 'The temporary base must have an existing parent.'
        }
        $createdTempBase = $true
        $null = [IO.Directory]::CreateDirectory($tempBase)
    }
    Add-OwnedDirectory -Path $testRoot -Boundary $tempBase
    foreach ($relative in @('repo', 'repo/member', 'repo/member/src', 'repo/third_party',
        'repo/third_party/vendor', 'repo/third_party/vendor/src', 'repo-sibling', 'repo-sibling/src',
        'fingerprint-repo', 'fingerprint-repo/crates', 'fingerprint-repo/crates/fixture',
        'fingerprint-repo/crates/fixture/src', 'fingerprint-repo/app', 'fingerprint-repo/app/.dart_tool',
        'fingerprint-repo/third_party', 'flutter-metadata-fixture', 'flutter-metadata-fixture/bin',
        'flutter-metadata-fixture/bin/cache', 'empty-git-template', 'stage', 'stage/cua', 'cargo-target')) {
        Add-OwnedDirectory -Path (Join-Path $testRoot $relative) -Boundary $testRoot
    }
    Register-OwnedFile (Join-Path $targetDirectory '.rustc_info.json')
    Write-FixtureFile $emptyGitConfig ''
    foreach ($name in $savedEnvironment.Keys) { [Environment]::SetEnvironmentVariable($name, $null, 'Process') }
    [Environment]::SetEnvironmentVariable('CARGO_NET_OFFLINE', 'true', 'Process')
    [Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', $targetDirectory, 'Process')
    [Environment]::SetEnvironmentVariable('FLUTTER_ROOT', $flutterFixture, 'Process')
    [Environment]::SetEnvironmentVariable('GIT_CONFIG_GLOBAL', $emptyGitConfig, 'Process')
    [Environment]::SetEnvironmentVariable('GIT_CONFIG_NOSYSTEM', '1', 'Process')
    Write-FixtureFile $fixtureManifest @'
[workspace]
members = ["member"]
exclude = ["third_party/vendor"]
resolver = "2"
'@
    Write-FixtureFile (Join-Path $fixtureRoot 'member/Cargo.toml') @'
[package]
name = "sbom-fixture-member"
version = "0.0.0"
edition = "2021"
[dependencies]
sbom-fixture-vendor = { path = "../third_party/vendor" }
sbom-fixture-external = { path = "../../repo-sibling" }
'@
    $vendorHeader = "[package]`nname = `"sbom-fixture-vendor`"`nversion = `"0.0.0`"`nedition = `"2021`"`n"
    Write-FixtureFile $vendorManifest $vendorHeader
    Write-FixtureFile (Join-Path $siblingRoot 'Cargo.toml') @'
[package]
name = "sbom-fixture-external"
version = "0.0.0"
edition = "2021"
'@
    foreach ($relative in @('repo/member/src/lib.rs', 'repo/third_party/vendor/src/lib.rs', 'repo-sibling/src/lib.rs')) {
        Write-FixtureFile (Join-Path $testRoot $relative) '// 仅供 Cargo metadata 发现 target；不会编译。'
    }
    Register-OwnedFile (Join-Path $fixtureRoot 'Cargo.lock')
    $metadata = Get-FixtureMetadata -Initial
    $member = Get-FixturePackage $metadata 'sbom-fixture-member'
    $vendor = Get-FixturePackage $metadata 'sbom-fixture-vendor'
    $external = Get-FixturePackage $metadata 'sbom-fixture-external'
    $members = @($metadata.workspace_members)
    Assert-Check ($metadata.packages.Count -eq 3 -and $members.Count -eq 1 -and
        $members -ccontains $member.id -and $members -cnotcontains $vendor.id -and
        $members -cnotcontains $external.id) 'Real Cargo workspace membership must exclude both path dependencies.'
    Assert-Check ($null -eq $member.source -and $null -eq $vendor.source -and $null -eq $external.source) `
        'All three real local packages must have Cargo source=null.'
    Assert-SourceProperties @(Get-CortexSbomCargoSourceProperties $member $members $fixtureRoot) `
        ([ordered]@{ 'cortex:source' = 'workspace' }) 'Workspace member'
    $localExpected = [ordered]@{
        'cortex:source' = 'local-path'
        'cortex:local-source-manifest' = 'third_party/vendor/Cargo.toml'
    }
    Assert-SourceProperties @(Get-CortexSbomCargoSourceProperties $vendor $members $fixtureRoot) `
        $localExpected 'Excluded vendor without optional provenance'
    Assert-SourceProperties @(Get-CortexSbomCargoSourceProperties $member @() $fixtureRoot) `
        ([ordered]@{ 'cortex:source' = 'local-path'; 'cortex:local-source-manifest' = 'member/Cargo.toml' }) `
        'Membership must come from workspace_members rather than the directory alone'
    Assert-Check (([string]$external.manifest_path).StartsWith($fixtureRoot, [StringComparison]::OrdinalIgnoreCase) -and
        -not ([string]$external.manifest_path).StartsWith($fixtureRoot + '\', [StringComparison]::OrdinalIgnoreCase)) `
        'External fixture must exercise the sibling-prefix boundary trap.'
    $externalProperties = @(Get-CortexSbomCargoSourceProperties $external $members $fixtureRoot)
    Assert-SourceProperties $externalProperties ([ordered]@{ 'cortex:source' = 'external-local-path' }) `
        'External sibling-prefix dependency'
    Assert-Check (-not ($externalProperties | ConvertTo-Json -Compress).Contains($testRoot)) `
        'External source properties must not expose the absolute fixture directory.'

    # 远程 source 的透传单独使用字段 fixture；不伪称离线下载了 registry/git 包。
    foreach ($remoteSource in @('registry+https://github.com/rust-lang/crates.io-index',
        ('git+https://example.invalid/repo?rev=release#' + ('c' * 40)))) {
        $remote = [pscustomobject]@{ id = $member.id; source = $remoteSource }
        Assert-SourceProperties @(Get-CortexSbomCargoSourceProperties $remote $members $fixtureRoot) `
            ([ordered]@{ 'cortex:source' = $remoteSource }) 'Remote source passthrough, ahead of membership'
    }
    foreach ($revisionLength in @(40, 64)) {
        $revision = 'b' * $revisionLength
        Write-FixtureFile $vendorManifest ($vendorHeader + (ConvertTo-VendorToml -Overrides @{
            native_backport_revision = '"' + $revision + '"'
        }))
        $validMetadata = Get-FixtureMetadata
        $validVendor = Get-FixturePackage $validMetadata 'sbom-fixture-vendor'
        $expected = [ordered]@{
            'cortex:source' = 'local-path'
            'cortex:local-source-manifest' = 'third_party/vendor/Cargo.toml'
            'cortex:upstream-crate-sha256' = 'a' * 64
            'cortex:native-backport-revision' = $revision
            'cortex:native-backport-url' = 'https://example.invalid/upstream/commit/backport'
        }
        Assert-SourceProperties @(Get-CortexSbomCargoSourceProperties $validVendor @($validMetadata.workspace_members) $fixtureRoot) `
            $expected "Valid vendor provenance with a $revisionLength-hex revision"
    }

    $invalidCases = New-Object 'System.Collections.Generic.List[object]'
    foreach ($field in @('upstream_crate_sha256', 'native_backport_revision', 'native_backport_url')) {
        $invalidCases.Add(@{ label = "missing $field"; toml = ConvertTo-VendorToml -Omit @($field); reason = 'incomplete' })
        foreach ($bad in @('7', '""', '" "')) {
            $override = @{}
            $override[$field] = $bad
            $invalidCases.Add(@{ label = "$field = $bad"; toml = ConvertTo-VendorToml -Overrides $override; reason = 'incomplete' })
        }
    }
    foreach ($checksum in @(('a' * 63), ('a' * 65), ('a' * 63 + 'g'))) {
        $invalidCases.Add(@{ label = "invalid checksum $($checksum.Length)"; reason = 'invalid';
            toml = ConvertTo-VendorToml -Overrides @{ upstream_crate_sha256 = '"' + $checksum + '"' } })
    }
    foreach ($revision in @(('b' * 39), ('b' * 41), ('b' * 63), ('b' * 65), ('b' * 39 + 'g'))) {
        $invalidCases.Add(@{ label = "invalid revision $($revision.Length)"; reason = 'invalid';
            toml = ConvertTo-VendorToml -Overrides @{ native_backport_revision = '"' + $revision + '"' } })
    }
    foreach ($url in @('http://example.invalid/backport', '/relative/backport', '//example.invalid/backport',
        'https://user:password@example.invalid/backport', 'https://example.invalid/backport?token=fixture',
        'https://example.invalid/backport#fragment', 'https://')) {
        $invalidCases.Add(@{ label = "unsafe URL $url"; reason = 'invalid';
            toml = ConvertTo-VendorToml -Overrides @{ native_backport_url = '"' + $url + '"' } })
    }
    foreach ($table in @('[package.metadata.cortex_vendor]',
        "[package.metadata]`ncortex_vendor = `"wrong-type`"",
        "[package.metadata]`ncortex_vendor = []")) {
        $invalidCases.Add(@{ label = 'empty or wrong-type provenance'; toml = $table; reason = 'incomplete' })
    }
    foreach ($case in $invalidCases) {
        Write-FixtureFile $vendorManifest ($vendorHeader + $case.toml + "`n")
        $invalidMetadata = Get-FixtureMetadata
        $invalidVendor = Get-FixturePackage $invalidMetadata 'sbom-fixture-vendor'
        $rejected = $false
        try {
            $null = Get-CortexSbomCargoSourceProperties $invalidVendor @($invalidMetadata.workspace_members) $fixtureRoot
        }
        catch {
            $message = $_.Exception.Message
            if (-not $message.StartsWith("Vendor provenance is $($case.reason): sbom-fixture-vendor", [StringComparison]::Ordinal) -or
                $message.Contains($testRoot)) { throw }
            $rejected = $true
        }
        Assert-Check $rejected "Malformed real Cargo provenance must be rejected: $($case.label)"
        $script:rejectedMetadataCases++
    }
    Assert-NoBuildArtifacts
    Write-Output "Cargo source fixture passed: $script:metadataRuns offline metadata calls, $script:rejectedMetadataCases rejected provenance cases."

    # fingerprint 用另一份最小源码树：不依赖主 Cargo.lock、Pub cache、Flutter 或 CRT 安装。
    Write-FixtureFile (Join-Path $fingerprintRoot 'Cargo.toml') @'
[workspace]
members = ["crates/fixture"]
resolver = "2"
'@
    Write-FixtureFile (Join-Path $fingerprintRoot 'crates/fixture/Cargo.toml') @'
[package]
name = "sbom-fingerprint-member"
version = "0.0.0"
edition = "2021"
license = "Apache-2.0"
'@
    Write-FixtureFile (Join-Path $fingerprintRoot 'crates/fixture/src/lib.rs') '// Metadata fixture only; never compiled.'
    Register-OwnedFile (Join-Path $fingerprintRoot 'Cargo.lock')
    Push-Location -LiteralPath $testRoot
    try {
        $fingerprintMetadata = Invoke-CortexNativeUtf8Json -FilePath 'cargo' -ArgumentList @('metadata',
            '--manifest-path', (Join-Path $fingerprintRoot 'Cargo.toml'), '--offline', '--format-version', '1')
    }
    finally { Pop-Location }
    $script:metadataRuns++
    Assert-Check ($fingerprintMetadata.packages.Count -eq 1 -and
        $fingerprintMetadata.packages[0].name -ceq 'sbom-fingerprint-member' -and
        $fingerprintMetadata.workspace_members.Count -eq 1) 'Fingerprint Cargo fixture must resolve without remote dependencies.'
    Write-FixtureFile (Join-Path $fingerprintRoot 'app/pubspec.yaml') @'
name: cortex_app
version: 0.0.0-sbom-source-selftest
environment:
  sdk: '>=3.0.0 <4.0.0'
'@
    Write-FixtureFile (Join-Path $fingerprintRoot 'app/.dart_tool/package_config.json') @'
{"configVersion":2,"packages":[{"name":"cortex_app","rootUri":"../","packageUri":"lib/","languageVersion":"3.0"}]}
'@
    Write-FixtureFile (Join-Path $flutterFixture 'bin/cache/flutter.version.json') (([ordered]@{
        frameworkVersion = '0.0.0-sbom-source-selftest'
        frameworkRevision = '1' * 40
        engineRevision = '2' * 40
        engineContentHash = '3' * 40
        dartSdkVersion = '0.0.0-sbom-source-selftest'
    }) | ConvertTo-Json)
    foreach ($placeholder in @('cortex_app.exe', 'cortex-local.exe', 'vcruntime140.dll')) {
        Write-FixtureFile (Join-Path $stage $placeholder) 'SBOM METADATA FIXTURE ONLY. This is text, not an executable.'
    }
    $stageSpdx = Join-Path $stage 'cua/SBOM.spdx.json'
    Register-OwnedFile $stageSpdx
    [IO.File]::Copy((Join-Path $source 'third_party/cua/SBOM.spdx.json'), $stageSpdx, $false)
    $stageHashesBefore = @(Get-ChildItem -LiteralPath $stage -File -Recurse | Sort-Object FullName |
        ForEach-Object { (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }) -join ','

    $null = Invoke-FixtureGit @('init', '--quiet', '--template', $emptyGitTemplate, '--initial-branch=codex/sbom-source-selftest')
    $null = Invoke-FixtureGit @('add', '--', 'Cargo.toml', 'Cargo.lock', 'crates', 'app')
    $null = Invoke-FixtureGit @('commit', '--quiet', '-m', 'SBOM metadata fixture baseline')
    $null = Invoke-FixtureGit @('tag', 'sbom-source-fixture-untracked')
    Assert-Check (@(Invoke-FixtureGit @('status', '--porcelain')).Count -eq 0) 'The private fingerprint fixture must start from a clean Git commit.'
    Add-OwnedDirectory -Path $probeDirectory -Boundary $thirdParty
    $probeContent = "/* SBOM fingerprint fixture; never compiled. */`nint cortex_sbom_source_regression = 1;`n"
    Write-FixtureFile $probePath $probeContent
    foreach ($tracking in @('untracked', 'tracked')) {
        if ($tracking -ceq 'tracked') {
            $null = Invoke-FixtureGit @('add', '--', $probeRelative)
            $null = Invoke-FixtureGit @('commit', '--quiet', '-m', 'Track private non-Cua C probe')
            $null = Invoke-FixtureGit @('tag', 'sbom-source-fixture-tracked')
            # 两次都保持 dirty，避免 dirty 位变化干扰内容变化这一核心断言。
            Write-FixtureFile $probePath ($probeContent.Replace(' = 1;', ' = 3;'))
        }
        $untracked = @(Invoke-FixtureGit @('ls-files', '--others', '--exclude-standard', '--', $probeRelative))
        $tracked = @(Invoke-FixtureGit @('ls-files', '--cached', '--', $probeRelative))
        if ($tracking -ceq 'untracked') {
            Assert-Check ($untracked.Count -eq 1 -and $untracked[0].Replace('\', '/') -ceq $probeRelative -and
                $tracked.Count -eq 0) 'The non-Cua probe must exercise the real untracked-input branch.'
        } else {
            Assert-Check ($tracked.Count -eq 1 -and $tracked[0].Replace('\', '/') -ceq $probeRelative -and
                $untracked.Count -eq 0) 'The same non-Cua probe must exercise the real tracked-diff branch.'
        }
        $frozenBefore = Get-FrozenInputState
        $first = Invoke-FullSbom (Join-Path $testRoot "$tracking-before.cdx.json")
        $probeBefore = Get-Item -LiteralPath $probePath
        $probeLength = $probeBefore.Length
        $probeTime = $probeBefore.LastWriteTimeUtc
        $probeHashBefore = (Get-FileHash -LiteralPath $probePath -Algorithm SHA256).Hash
        $nextValue = if ($tracking -ceq 'untracked') { '2' } else { '4' }
        Write-FixtureFile $probePath ($probeContent.Replace(' = 1;', " = $nextValue;"))
        [IO.File]::SetLastWriteTimeUtc($probePath, $probeTime)
        $probeAfter = Get-Item -LiteralPath $probePath
        Assert-Check ($probeAfter.Length -eq $probeLength -and $probeAfter.LastWriteTimeUtc -eq $probeTime -and
            (Get-FileHash -LiteralPath $probePath -Algorithm SHA256).Hash -cne $probeHashBefore) `
            'The C probe must change only content, preserving its name, length and timestamp.'
        $second = Invoke-FullSbom (Join-Path $testRoot "$tracking-after.cdx.json")
        Assert-Check ((Get-FrozenInputState) -ceq $frozenBefore) `
            'All other fixture build inputs must stay byte-identical within each full-generator pair.'
        Assert-FingerprintPair $first $second $tracking
    }
    $stageHashesAfter = @(Get-ChildItem -LiteralPath $stage -File -Recurse | Sort-Object FullName |
        ForEach-Object { (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }) -join ','
    Assert-Check ($stageHashesAfter -ceq $stageHashesBefore) 'Private stage files must remain byte-identical.'
    $productionHashesAfter = @($productionInputs | ForEach-Object {
        (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash
    }) -join ','
    Assert-Check ($productionHashesAfter -ceq $productionHashesBefore) 'The actual production generator, helpers and Cua SPDX must remain byte-identical.'
    Assert-NoBuildArtifacts
}
finally {
    foreach ($name in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
    }
    $cleanupErrors = New-Object 'System.Collections.Generic.List[string]'
    for ($index = $ownedFiles.Count - 1; $index -ge 0; $index--) {
        $path = $ownedFiles[$index]
        try {
            if ($path -ieq $probePath) {
                Assert-SafePath -Path $probeDirectory -Boundary $thirdParty
                Assert-SafePath -Path $path -Boundary $probeDirectory
            } else { Assert-SafePath -Path $path -Boundary $testRoot }
            if ([IO.File]::Exists($path)) {
                # Git loose object 在 Windows 上只读；边界核验后仅清除此自有文件的只读位。
                $attributes = [IO.File]::GetAttributes($path)
                if (($attributes -band [IO.FileAttributes]::ReadOnly) -ne 0) {
                    [IO.File]::SetAttributes($path, $attributes -band (-bnot [IO.FileAttributes]::ReadOnly))
                }
                [IO.File]::Delete($path)
            }
        }
        catch { $cleanupErrors.Add("$path : $($_.Exception.Message)") }
    }
    for ($index = $ownedDirectories.Count - 1; $index -ge 0; $index--) {
        $path = $ownedDirectories[$index]
        try {
            if ($path -ieq $probeDirectory) { Assert-SafePath -Path $path -Boundary $thirdParty }
            elseif ($path -ieq $testRoot) { Assert-SafePath -Path $path -Boundary $tempBase }
            else { Assert-SafePath -Path $path -Boundary $testRoot }
            if ([IO.Directory]::Exists($path)) { [IO.Directory]::Delete($path, $false) }
        }
        catch { $cleanupErrors.Add("$path : $($_.Exception.Message)") }
    }
    if ($createdTempBase) {
        try {
            Assert-SafePath -Path $tempBase -Boundary ([IO.Path]::GetPathRoot($tempBase))
            if ([IO.Directory]::Exists($tempBase) -and
                @(Get-ChildItem -LiteralPath $tempBase -Force).Count -eq 0) {
                [IO.Directory]::Delete($tempBase, $false)
            }
        }
        catch { $cleanupErrors.Add($_.Exception.Message) }
    }
    if ($cleanupErrors.Count -gt 0) { throw ('Exact fixture cleanup failed: ' + ($cleanupErrors -join '; ')) }
}
Assert-Check (-not (Test-Path -LiteralPath $probeDirectory) -and -not (Test-Path -LiteralPath $testRoot)) `
    'The private Git/Cargo/app/SDK fixtures and their probe must be removed.'
$changedEnvironment = @($savedEnvironment.Keys | Where-Object {
    [Environment]::GetEnvironmentVariable($_, 'Process') -cne $savedEnvironment[$_]
})
Assert-Check ($changedEnvironment.Count -eq 0) 'Cargo, Flutter and Git environment variables must be restored after the fixture.'
Write-Output ("SBOM source self-test passed: {0} checks; {1} offline Cargo fixture calls; {2} invalid metadata rejections; {3} full generator calls. Fixtures only: no build, signing or desktop acceptance." -f
    $script:checks, $script:metadataRuns, $script:rejectedMetadataCases, $script:generatorRuns)
