param(
    [string]$OutputPath = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$fixtureRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$sourcePath = Join-Path $fixtureRoot "src\Program.cs"
$manifestPath = Join-Path $fixtureRoot "app.manifest"
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    throw "The fixture application manifest was not found: $manifestPath"
}
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $fixtureRoot "target\cortex-computer-use-windows-fixture.exe"
}
$OutputPath = [System.IO.Path]::GetFullPath($OutputPath)

$frameworkRoot = Join-Path $env:WINDIR "Microsoft.NET"
$compilerCandidates = @(
    (Join-Path $frameworkRoot "Framework64\v4.0.30319\csc.exe"),
    (Join-Path $frameworkRoot "Framework\v4.0.30319\csc.exe")
)
$csc = $compilerCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if ($null -eq $csc) {
    throw "The .NET Framework C# compiler (csc.exe) was not found."
}
$is64Bit = $csc -match "Framework64"
$platform = if ($is64Bit) { "x64" } else { "x86" }
$gacRoot = Join-Path $frameworkRoot "assembly"

function Find-GacAssembly {
    param(
        [string]$Name,
        [string]$Bucket
    )

    $assemblyRoot = Get-ChildItem -LiteralPath (Join-Path $gacRoot $Bucket) `
        -Directory -Filter $Name -ErrorAction SilentlyContinue |
        Select-Object -First 1
    $assembly = if ($null -eq $assemblyRoot) {
        $null
    }
    else {
        Get-ChildItem -LiteralPath $assemblyRoot.FullName -Recurse -Filter "$Name.dll" `
            -ErrorAction SilentlyContinue |
            Select-Object -First 1
    }
    if ($null -eq $assembly) {
        throw "Required .NET Framework WPF assembly not found: $Name ($Bucket)"
    }
    return $assembly.FullName
}

$presentationFramework = Find-GacAssembly -Name "PresentationFramework" -Bucket "GAC_MSIL"
$presentationCore = Find-GacAssembly -Name "PresentationCore" -Bucket $(if ($is64Bit) { "GAC_64" } else { "GAC_32" })
$windowsBase = Find-GacAssembly -Name "WindowsBase" -Bucket "GAC_MSIL"
$systemXaml = Find-GacAssembly -Name "System.Xaml" -Bucket "GAC_MSIL"

$outputDirectory = Split-Path -Parent $OutputPath
$null = New-Item -ItemType Directory -Path $outputDirectory -Force

$compilerArguments = @(
    "/nologo"
    "/target:winexe"
    "/platform:$platform"
    "/optimize+"
    "/debug-"
    "/warnaserror+"
    "/out:$OutputPath"
    "/win32manifest:$manifestPath"
    "/reference:$presentationFramework"
    "/reference:$presentationCore"
    "/reference:$windowsBase"
    "/reference:$systemXaml"
    $sourcePath
)

& $csc @compilerArguments
if ($LASTEXITCODE -ne 0) {
    throw "C# compilation failed with exit code $LASTEXITCODE"
}

$hash = (Get-FileHash -LiteralPath $OutputPath -Algorithm SHA256).Hash
Write-Output "Built: $OutputPath"
Write-Output "SHA256: $hash"
