param(
    [Parameter(Mandatory = $true)][string]$StageDirectory,
    [Parameter(Mandatory = $true)][string]$OutputPath,
    [string]$SourceRoot = "",
    [string]$Version = "",
    [string]$InstallerPath = "",
    [string]$SourceFingerprint = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
foreach ($module in @("Microsoft.PowerShell.Utility", "Microsoft.PowerShell.Management")) {
    $modulePath = [IO.Path]::Combine($PSHOME, "Modules", $module, "$module.psd1")
    Import-Module -Name $modulePath -ErrorAction Stop
}

$generatorVersion = "cortex-cyclonedx-generator/1"
$stage = (Resolve-Path -LiteralPath $StageDirectory).ProviderPath
if (-not (Test-Path -LiteralPath $stage -PathType Container)) {
    throw "Stage directory is missing: $StageDirectory"
}
if ([string]::IsNullOrWhiteSpace($SourceRoot)) {
    $SourceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
}
$source = (Resolve-Path -LiteralPath $SourceRoot).ProviderPath
$appRoot = Join-Path $source "app"
$cargoToml = Join-Path $source "Cargo.toml"
$packageConfigPath = Join-Path $appRoot ".dart_tool\package_config.json"
$pubspecPath = Join-Path $appRoot "pubspec.yaml"
$cuaSpdxPath = Join-Path $stage "cua\SBOM.spdx.json"
$output = [IO.Path]::GetFullPath($OutputPath)
$flutterRoot = $env:FLUTTER_ROOT
if ([string]::IsNullOrWhiteSpace($flutterRoot)) {
    $flutterCommand = Get-Command flutter -ErrorAction SilentlyContinue
    if ($null -eq $flutterCommand) {
        throw "Flutter is required to identify the framework and Dart SDK in the SBOM."
    }
    $flutterRoot = Split-Path (Split-Path $flutterCommand.Source -Parent) -Parent
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $match = Select-String -LiteralPath $cargoToml `
        -Pattern '^version\s*=\s*"([^"]+)"$' |
        Select-Object -First 1
    if ($null -eq $match) {
        throw "Could not read the workspace version from $cargoToml"
    }
    $Version = $match.Matches[0].Groups[1].Value
}
foreach ($required in @(
    $cargoToml,
    $packageConfigPath,
    $pubspecPath,
    (Join-Path $stage "cortex_app.exe"),
    (Join-Path $stage "cortex-local.exe"),
    $cuaSpdxPath
)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "SBOM input is missing: $required"
    }
}
if (
    -not [string]::IsNullOrWhiteSpace($InstallerPath) -and
    -not (Test-Path -LiteralPath $InstallerPath -PathType Leaf)
) {
    throw "Installer is missing: $InstallerPath"
}

function Get-Sha256 {
    param([string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-StringSha256 {
    param([string]$Value)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return (
            [BitConverter]::ToString(
                $sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Value))
            )
        ).Replace("-", "").ToLowerInvariant()
    }
    finally {
        $sha.Dispose()
    }
}

function New-StableRef {
    param(
        [string]$Kind,
        [string]$Identity
    )
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes("$Kind`0$Identity")
        $digest = ([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace("-", "").ToLowerInvariant()
        return "urn:cortex:$Kind`:$digest"
    }
    finally {
        $sha.Dispose()
    }
}

function New-StableUuid {
    param([string]$Identity)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $hashBytes = $sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Identity))
        $uuidBytes = New-Object byte[] 16
        [Array]::Copy($hashBytes, $uuidBytes, 16)
        $uuidBytes[6] = [byte](($uuidBytes[6] -band 0x0F) -bor 0x50)
        $uuidBytes[8] = [byte](($uuidBytes[8] -band 0x3F) -bor 0x80)
        return ([guid]::new($uuidBytes)).ToString()
    }
    finally {
        $sha.Dispose()
    }
}

function New-LicenseChoice {
    param([string]$Expression)
    if ([string]::IsNullOrWhiteSpace($Expression) -or $Expression -eq "NOASSERTION") {
        return @{ license = @{ name = "NOASSERTION" } }
    }
    if ($Expression -match '^[A-Za-z0-9.+\-]+(\s+(AND|OR|WITH)\s+[A-Za-z0-9.+\-]+)*$') {
        return @{ expression = $Expression }
    }
    return @{ license = @{ name = $Expression } }
}

function Find-PackageLicense {
    param([string]$Root)
    foreach ($name in @(
        "LICENSE",
        "LICENSE.md",
        "LICENSE.txt",
        "COPYING",
        "COPYING.md",
        "UNLICENSE"
    )) {
        $path = Join-Path $Root $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            continue
        }
        $text = Get-Content -LiteralPath $path -Raw
        $expression = "NOASSERTION"
        if ($text -match '(?im)^\s*Apache License\s*$' -and $text -match 'Version 2\.0') {
            $expression = "Apache-2.0"
        }
        elseif ($text -match 'Mozilla Public License Version 2\.0') {
            $expression = "MPL-2.0"
        }
        elseif ($text -match 'Permission is hereby granted, free of charge') {
            $expression = "MIT"
        }
        elseif ($text -match 'Redistribution and use in source and binary forms' -and
                $text -match 'Neither the name') {
            $expression = "BSD-3-Clause"
        }
        elseif ($text -match 'Redistribution and use in source and binary forms') {
            $expression = "BSD-2-Clause"
        }
        elseif ($text -match 'Permission to use, copy, modify, and/or distribute') {
            $expression = "ISC"
        }
        return [ordered]@{
            expression = $expression
            evidencePath = $path
            sha256 = Get-Sha256 $path
        }
    }
    return [ordered]@{
        expression = "NOASSERTION"
        evidencePath = ""
        sha256 = ""
    }
}

function Read-PubspecVersion {
    param([string]$Path)
    $match = Select-String -LiteralPath $Path -Pattern '^version:\s*([^+\s]+)' |
        Select-Object -First 1
    if ($null -eq $match) {
        return ""
    }
    return $match.Matches[0].Groups[1].Value
}

function Add-ComponentProperty {
    param(
        [hashtable]$Component,
        [string]$Name,
        [string]$Value
    )
    if (-not $Component.ContainsKey("properties")) {
        $Component.properties = @()
    }
    $Component.properties += @{
        name = $Name
        value = $Value
    }
}

$gitCommit = ""
$gitTag = ""
$gitDirty = $false
$gitCommand = Get-Command git -ErrorAction SilentlyContinue
if ($null -ne $gitCommand) {
    $gitCommit = (& git -C $source rev-parse HEAD 2>$null | Select-Object -First 1)
    $gitTag = (& git -C $source describe --tags --exact-match HEAD 2>$null | Select-Object -First 1)
    $status = @(& git -C $source status --porcelain --untracked-files=normal 2>$null)
    $gitDirty = $status.Count -gt 0
}
$sourceFingerprintScope = 'external-caller-supplied'
if ([string]::IsNullOrWhiteSpace($SourceFingerprint)) {
    $sourceFingerprintScope = 'tracked-build-diff,untracked-build-inputs,lockfiles'
    $fingerprintInputs = @(
        $cargoToml,
        (Join-Path $source "Cargo.lock"),
        $pubspecPath,
        (Join-Path $appRoot "pubspec.lock")
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf }
    $fingerprintLines = foreach ($path in $fingerprintInputs) {
        "$([IO.Path]::GetFileName($path)) $(Get-Sha256 $path)"
    }
    $diffText = ""
    $untrackedLines = @()
    $untracked = @()
    if ($null -ne $gitCommand) {
        $gitErrorAction = $ErrorActionPreference
        try {
            $ErrorActionPreference = "Continue"
            $diffText = (& git -C $source diff --binary HEAD -- `
                Cargo.toml Cargo.lock rust-toolchain.toml .cargo crates app scripts `
                third_party LICENSE NOTICE CHANGELOG.md 2>$null | Out-String)
            $untracked = @(
                & git -C $source ls-files --others --exclude-standard 2>$null
            )
        }
        finally {
            $ErrorActionPreference = $gitErrorAction
        }
        foreach ($relative in $untracked | Sort-Object) {
            $normalized = $relative.Replace("\", "/")
            if ($normalized -notmatch (
                '^(Cargo\.toml|Cargo\.lock|rust-toolchain\.toml|LICENSE|NOTICE|CHANGELOG\.md)$|' +
                '^(\.cargo|crates|app|scripts|third_party)/'
            )) {
                continue
            }
            $path = Join-Path $source $relative
            if (Test-Path -LiteralPath $path -PathType Leaf) {
                $untrackedLines += "$relative $(Get-Sha256 $path)"
            }
        }
    }
    $SourceFingerprint = Get-StringSha256 (@(
        "git-commit=$gitCommit"
        "tracked-diff-sha256=$(Get-StringSha256 $diffText)"
        ($fingerprintLines -join "`n")
        ($untrackedLines -join "`n")
    ) -join "`n")
}

$mainRef = "urn:cortex:application:cortex-desktop"
$components = @()

. (Join-Path $PSScriptRoot 'windows\native-json.ps1')
$cargoMetadata = Invoke-CortexNativeUtf8Json -FilePath 'cargo' -ArgumentList @(
    'metadata', '--manifest-path', $cargoToml, '--locked', '--format-version', '1',
    '--filter-platform', 'x86_64-pc-windows-msvc'
)
. (Join-Path $PSScriptRoot 'windows\sbom-cargo-source.ps1')
foreach ($package in $cargoMetadata.packages | Sort-Object id) {
    $ref = New-StableRef -Kind "cargo" -Identity ([string]$package.id)
    $component = [ordered]@{
        type = "library"
        "bom-ref" = $ref
        name = [string]$package.name
        version = [string]$package.version
        purl = if ([string]$package.source -like "registry+*") {
            "pkg:cargo/$($package.name)@$($package.version)"
        } else {
            $null
        }
        licenses = @((New-LicenseChoice -Expression ([string]$package.license)))
        properties = @(
            @{ name = "cortex:ecosystem"; value = "cargo" }
        ) + @(Get-CortexSbomCargoSourceProperties -Package $package -WorkspaceMemberIds @($cargoMetadata.workspace_members) -SourceRoot $source)
    }
    if ($null -eq $component.purl) {
        $component.Remove("purl")
    }
    $components += $component
}

$packageConfig = Get-Content -LiteralPath $packageConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json
$dartCount = 0
foreach ($package in $packageConfig.packages | Sort-Object name) {
    $rootUri = [string]$package.rootUri
    $rootPath = if ($rootUri.StartsWith("file:")) {
        ([Uri]$rootUri).LocalPath
    } else {
        [IO.Path]::GetFullPath((Join-Path (Split-Path $packageConfigPath -Parent) $rootUri))
    }
    $pubspec = Join-Path $rootPath "pubspec.yaml"
    $packageVersion = if (Test-Path -LiteralPath $pubspec -PathType Leaf) {
        Read-PubspecVersion $pubspec
    } else {
        ""
    }
    if ([string]::IsNullOrWhiteSpace($packageVersion)) {
        $packageVersion = "0.0.0-unknown"
    }
    $license = Find-PackageLicense $rootPath
    if ([string]$package.name -ieq "cortex_app") {
        $license = Find-PackageLicense $source
        $license.expression = "Apache-2.0"
    }
    elseif (
        $rootPath.StartsWith(
            (Join-Path $flutterRoot "packages") + [IO.Path]::DirectorySeparatorChar,
            [StringComparison]::OrdinalIgnoreCase
        )
    ) {
        $license = Find-PackageLicense $flutterRoot
        $license.expression = "BSD-3-Clause"
    }
    $properties = @(
        @{ name = "cortex:ecosystem"; value = "pub" }
    )
    if (-not [string]::IsNullOrWhiteSpace($license.evidencePath)) {
        $properties += @{
            name = "cortex:license-evidence-sha256"
            value = $license.sha256
        }
    }
    $components += [ordered]@{
        type = if ([string]$package.name -eq "cortex_app") { "application" } else { "library" }
        "bom-ref" = New-StableRef -Kind "pub" -Identity "$($package.name)@$packageVersion"
        name = [string]$package.name
        version = $packageVersion
        purl = "pkg:pub/$($package.name)@$packageVersion"
        licenses = @((New-LicenseChoice -Expression $license.expression))
        properties = $properties
    }
    $dartCount++
}

$flutterVersionPath = Join-Path $flutterRoot "bin\cache\flutter.version.json"
if (-not (Test-Path -LiteralPath $flutterVersionPath -PathType Leaf)) {
    throw "Flutter version metadata is missing: $flutterVersionPath"
}
$flutter = Get-Content -LiteralPath $flutterVersionPath -Raw -Encoding UTF8 | ConvertFrom-Json
$components += [ordered]@{
    type = "framework"
    "bom-ref" = New-StableRef -Kind "framework" -Identity "flutter@$($flutter.frameworkRevision)"
    name = "Flutter"
    version = [string]$flutter.frameworkVersion
    scope = "required"
    purl = "pkg:github/flutter/flutter@$($flutter.frameworkRevision)"
    licenses = @((New-LicenseChoice -Expression "BSD-3-Clause"))
    properties = @(
        @{ name = "cortex:engine-revision"; value = [string]$flutter.engineRevision },
        @{ name = "cortex:engine-content-hash"; value = [string]$flutter.engineContentHash }
    )
}
$components += [ordered]@{
    type = "framework"
    "bom-ref" = New-StableRef -Kind "framework" -Identity "dart@$($flutter.dartSdkVersion)"
    name = "Dart SDK"
    version = [string]$flutter.dartSdkVersion
    scope = "required"
    purl = "pkg:generic/dart-sdk@$($flutter.dartSdkVersion)"
    licenses = @((New-LicenseChoice -Expression "BSD-3-Clause"))
}

$cuaSpdx = Get-Content -LiteralPath $cuaSpdxPath -Raw -Encoding UTF8 | ConvertFrom-Json
foreach ($package in $cuaSpdx.packages) {
    $properties = @(
        @{ name = "cortex:ecosystem"; value = "cua-spdx" },
        @{ name = "cortex:upstream-spdx-id"; value = [string]$package.SPDXID }
    )
    $checksum = $null
    if ($null -ne $package.PSObject.Properties["checksums"]) {
        $checksum = $package.checksums | Select-Object -First 1
    }
    if ($null -ne $checksum) {
        $properties += @{
            name = "cortex:upstream-sha256"
            value = [string]$checksum.checksumValue
        }
    }
    $component = [ordered]@{
        type = "library"
        "bom-ref" = New-StableRef -Kind "cua" -Identity ([string]$package.SPDXID)
        name = [string]$package.name
        version = [string]$package.versionInfo
        scope = "required"
        licenses = @((New-LicenseChoice -Expression ([string]$package.licenseDeclared)))
        properties = $properties
    }
    $purlRef = $null
    if ($null -ne $package.PSObject.Properties["externalRefs"]) {
        $purlRef = $package.externalRefs |
            Where-Object { [string]$_.referenceType -eq "purl" } |
            Select-Object -First 1
    }
    if ($null -ne $purlRef) {
        $component.purl = [string]$purlRef.referenceLocator
    }
    $components += $component
}

$baseFileName = "vcruntime140.dll"
$crtVersion = (Get-Item -LiteralPath (Join-Path $stage "vcruntime140.dll")).VersionInfo.FileVersion
$components += [ordered]@{
    type = "library"
    "bom-ref" = New-StableRef -Kind "runtime" -Identity "msvc-runtime@$crtVersion"
    name = "Microsoft Visual C++ Redistributable Runtime"
    version = if ([string]::IsNullOrWhiteSpace($crtVersion)) { "NOASSERTION" } else { $crtVersion }
    scope = "required"
    licenses = @(@{ license = @{ name = "Microsoft Visual C++ Redistributable Terms" } })
    properties = @(
        @{ name = "cortex:redistributed-files"; value = "msvcp140.dll,vcruntime140.dll,vcruntime140_1.dll" },
        @{ name = "cortex:primary-file"; value = $baseFileName }
    )
}

$fileComponents = @()
$fileRefs = @()
$stagePrefix = $stage.TrimEnd("\", "/") + [IO.Path]::DirectorySeparatorChar
foreach ($file in Get-ChildItem -LiteralPath $stage -File -Recurse | Sort-Object FullName) {
    if ($file.FullName.Equals($output, [StringComparison]::OrdinalIgnoreCase)) {
        continue
    }
    $relative = $file.FullName.Substring($stagePrefix.Length).Replace("\", "/")
    $sha = Get-Sha256 $file.FullName
    $ref = New-StableRef -Kind "file" -Identity $relative
    $fileRefs += $ref
    $fileComponents += [ordered]@{
        type = "file"
        "bom-ref" = $ref
        name = [IO.Path]::GetFileName($relative)
        hashes = @(@{ alg = "SHA-256"; content = $sha })
        properties = @(
            @{ name = "cortex:path"; value = $relative },
            @{ name = "cortex:size"; value = [string]$file.Length }
        )
    }
}
$components += $fileComponents

if (-not [string]::IsNullOrWhiteSpace($InstallerPath)) {
    $installer = (Resolve-Path -LiteralPath $InstallerPath).ProviderPath
    $installerSha = Get-Sha256 $installer
    $installerRef = New-StableRef -Kind "file" -Identity ("installer/" + [IO.Path]::GetFileName($installer))
    $components += [ordered]@{
        type = "file"
        "bom-ref" = $installerRef
        name = [IO.Path]::GetFileName($installer)
        hashes = @(@{ alg = "SHA-256"; content = $installerSha })
        properties = @(
            @{ name = "cortex:role"; value = "windows-installer" },
            @{ name = "cortex:size"; value = [string](Get-Item -LiteralPath $installer).Length }
        )
    }
}

$components = @($components | Sort-Object { [string]$_["bom-ref"] })
$componentRefs = @{}
foreach ($component in $components) {
    $ref = [string]$component["bom-ref"]
    if ([string]::IsNullOrWhiteSpace($ref)) {
        throw "SBOM component has an empty bom-ref: $($component.name)"
    }
    if ($componentRefs.ContainsKey($ref)) {
        throw "SBOM contains a duplicate bom-ref: $ref"
    }
    $componentRefs[$ref] = $true
    if ($component.type -eq "file") {
        $hashes = @($component.hashes)
        if (
            $hashes.Count -ne 1 -or
            $hashes[0].alg -ne "SHA-256" -or
            $hashes[0].content -notmatch '^[0-9a-f]{64}$'
        ) {
            throw "SBOM file component has no valid SHA-256: $($component.name)"
        }
    }
    $componentProperties = if ($null -ne $component.PSObject.Properties["properties"]) {
        @($component.properties)
    } else {
        @()
    }
    foreach ($property in $componentProperties) {
        if ($null -eq $property) {
            continue
        }
        if (
            [string]$property.value -match '^[A-Za-z]:[\\/]' -or
            [string]$property.value -match '(?i)file:///(Users|home)/'
        ) {
            throw "SBOM property leaks a local path: $($property.name)"
        }
    }
}
$allRefs = @(
    $components |
        ForEach-Object { [string]$_["bom-ref"] }
)
$dependencyRefs = @{}
foreach ($ref in $allRefs) {
    if ($dependencyRefs.ContainsKey($ref)) {
        throw "SBOM dependency list contains a duplicate bom-ref: $ref"
    }
    $dependencyRefs[$ref] = $true
}
foreach ($name in @(
    "Flutter",
    "Dart SDK",
    "Cua Driver",
    "Microsoft Visual C++ Redistributable Runtime"
)) {
    $matchedComponents = @(
        $components |
            Where-Object {
                [string]::Equals(
                    [string]$_.name,
                    $name,
                    [StringComparison]::Ordinal
                )
            }
    )
    if ($matchedComponents.Count -ne 1) {
        $known = @(
            $components |
                Where-Object {
                    $_.name -match '(?i)Flutter|Dart|Cua|Microsoft Visual'
                } |
                ForEach-Object { [string]$_.name } |
                Sort-Object -Unique
        ) -join ", "
        throw "SBOM is missing the required component: $name (matching names: $known)"
    }
}
if (-not [string]::IsNullOrWhiteSpace($InstallerPath)) {
    $installerName = [IO.Path]::GetFileName(
        (Resolve-Path -LiteralPath $InstallerPath).ProviderPath
    )
    if (@($components | Where-Object {
        $_.type -eq "file" -and $_.name -eq $installerName
    }).Count -ne 1) {
        throw "Final SBOM does not contain the installer file component."
    }
}
$serialIdentity = @(
    "Cortex Desktop",
    $Version,
    $gitCommit,
    $SourceFingerprint,
    ($allRefs -join ",")
) -join "`n"
$serial = New-StableUuid $serialIdentity

$metadataProperties = @(
    @{ name = "cortex:generator-version"; value = $generatorVersion },
    @{ name = "cortex:source-fingerprint"; value = $SourceFingerprint },
    @{ name = "cortex:source-fingerprint-scope"; value = $sourceFingerprintScope },
    @{ name = "cortex:git-commit"; value = $gitCommit },
    @{ name = "cortex:git-tag"; value = $gitTag },
    @{ name = "cortex:git-dirty"; value = $gitDirty.ToString().ToLowerInvariant() },
    @{ name = "cortex:cargo-packages"; value = [string]$cargoMetadata.packages.Count },
    @{ name = "cortex:dart-packages"; value = [string]$dartCount },
    @{ name = "cortex:stage-files"; value = [string]$fileComponents.Count }
)
if ($SourceFingerprint -notmatch '^[0-9a-f]{64}$') {
    throw "Source fingerprint is not a SHA-256 digest: $SourceFingerprint"
}
if (-not [string]::IsNullOrWhiteSpace($gitCommit) -and $gitCommit -notmatch '^[0-9a-f]{40}$') {
    throw "Git commit is not a full SHA-1: $gitCommit"
}

$bom = [ordered]@{
    bomFormat = "CycloneDX"
    specVersion = "1.5"
    serialNumber = "urn:uuid:$serial"
    version = 1
    metadata = [ordered]@{
        timestamp = [DateTime]::UtcNow.ToString("o")
        tools = [ordered]@{
            components = @(
                [ordered]@{
                    type = "application"
                    name = "cortex-windows-sbom-generator"
                    version = "1"
                }
            )
        }
        authors = @(@{ name = "Cortex build pipeline" })
        component = [ordered]@{
            type = "application"
            "bom-ref" = $mainRef
            name = "Cortex Desktop"
            version = $Version
            scope = "required"
            licenses = @((New-LicenseChoice -Expression "Apache-2.0"))
            properties = $metadataProperties
            externalReferences = @(
                @{
                    type = "vcs"
                    url = "https://github.com/weironz/cortex"
                }
            )
        }
    }
    components = $components
    dependencies = @(
        [ordered]@{
            ref = $mainRef
            dependsOn = $allRefs
        }
    )
}

$outputDirectory = Split-Path -Parent $output
if (-not [string]::IsNullOrWhiteSpace($outputDirectory)) {
    $null = New-Item -ItemType Directory -Path $outputDirectory -Force
}
[IO.File]::WriteAllText(
    $output,
    ($bom | ConvertTo-Json -Depth 20) + [Environment]::NewLine,
    [Text.UTF8Encoding]::new($false)
)

[pscustomobject]@{
    output = $output
    sha256 = Get-Sha256 $output
    components = $components.Count
    cargoPackages = $cargoMetadata.packages.Count
    dartPackages = $dartCount
    files = $fileComponents.Count
    sourceFingerprint = $SourceFingerprint
    gitCommit = $gitCommit
    gitDirty = $gitDirty
} | Format-List
