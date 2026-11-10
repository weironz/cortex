$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$fixtureRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$artifactRoot = [System.IO.Path]::GetFullPath((Join-Path $fixtureRoot "artifacts"))
if (-not (Test-Path -LiteralPath $artifactRoot -PathType Container)) {
    Write-Output "No artifact directory exists: $artifactRoot"
    return
}

$artifactPrefix = $artifactRoot.TrimEnd("\") + "\"
$chromeProcesses = Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" |
    Where-Object {
        $_.CommandLine -and
        $_.CommandLine.IndexOf(
            $artifactRoot,
            [System.StringComparison]::OrdinalIgnoreCase
        ) -ge 0
    }

foreach ($process in $chromeProcesses) {
    $killInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $killInfo.FileName = "$env:WINDIR\System32\taskkill.exe"
    $killInfo.Arguments = "/PID $($process.ProcessId) /T /F"
    $killInfo.UseShellExecute = $false
    $killInfo.CreateNoWindow = $true
    $killer = [System.Diagnostics.Process]::Start($killInfo)
    $null = $killer.WaitForExit(5000)
    Write-Output "terminated test Chrome pid $($process.ProcessId)"
}

Start-Sleep -Seconds 1
$directories = Get-ChildItem -LiteralPath $artifactRoot -Directory -Force
foreach ($directory in $directories) {
    $fullPath = [System.IO.Path]::GetFullPath($directory.FullName)
    if (-not $fullPath.StartsWith(
            $artifactPrefix,
            [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove directory outside fixture artifacts: $fullPath"
    }
    Remove-Item -LiteralPath $fullPath -Recurse -Force
    Write-Output "removed $fullPath"
}

$files = Get-ChildItem -LiteralPath $artifactRoot -File -Force
foreach ($file in $files) {
    $fullPath = [System.IO.Path]::GetFullPath($file.FullName)
    if (-not $fullPath.StartsWith(
            $artifactPrefix,
            [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove file outside fixture artifacts: $fullPath"
    }
    Remove-Item -LiteralPath $fullPath -Force
    Write-Output "removed $fullPath"
}
