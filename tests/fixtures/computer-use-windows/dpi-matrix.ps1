param(
    [string]$OutputPath = "",
    [string]$ManifestPath = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$fixtureRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$fixtureExe = Join-Path $fixtureRoot "target\cortex-computer-use-windows-fixture.exe"
$driverExe = "D:\codes\_ref\cua-releases\v0.38.0\windows-x86_64\cua-driver.exe"
$driverSha256 = "ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2"

if (-not (Test-Path -LiteralPath $fixtureExe -PathType Leaf)) {
    throw "Fixture executable was not found: $fixtureExe"
}
if (-not (Test-Path -LiteralPath $driverExe -PathType Leaf)) {
    throw "Cua Driver executable was not found: $driverExe"
}
if ((Get-FileHash -LiteralPath $driverExe -Algorithm SHA256).Hash.ToLowerInvariant() -ne $driverSha256) {
    throw "Cua Driver SHA-256 does not match the pinned 0.38.0 release."
}
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $fixtureRoot "results\dpi-permonitorv2.json"
}
if ([string]::IsNullOrWhiteSpace($ManifestPath)) {
    $ManifestPath = Join-Path ([System.IO.Path]::GetTempPath()) "cortex-dpi-manifest-$([guid]::NewGuid().ToString('N')).json"
}

Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class CortexDpiMatrixNative {
    [StructLayout(LayoutKind.Sequential)] public struct Luid { public uint LowPart; public int HighPart; }
    [StructLayout(LayoutKind.Sequential)] public struct Rational { public uint Numerator; public uint Denominator; }
    [StructLayout(LayoutKind.Sequential)] public struct SourceInfo { public Luid adapterId; public uint id; public uint modeInfoIdx; public uint statusFlags; }
    [StructLayout(LayoutKind.Sequential)] public struct TargetInfo { public Luid adapterId; public uint id; public uint modeInfoIdx; public uint outputTechnology; public uint rotation; public uint scaling; public Rational refreshRate; public uint scanLineOrdering; public int targetAvailable; public uint statusFlags; }
    [StructLayout(LayoutKind.Sequential)] public struct PathInfo { public SourceInfo sourceInfo; public TargetInfo targetInfo; public uint flags; }
    [StructLayout(LayoutKind.Sequential)] public struct Header { public uint type; public uint size; public Luid adapterId; public uint id; }
    [StructLayout(LayoutKind.Sequential)] public struct ScaleGet { public Header header; public int minScaleRel; public int curScaleRel; public int maxScaleRel; }
    [StructLayout(LayoutKind.Sequential)] public struct ScaleSet { public Header header; public int scaleRel; }
    [DllImport("user32.dll")] static extern int GetDisplayConfigBufferSizes(uint flags, out uint paths, out uint modes);
    [DllImport("user32.dll")] static extern int QueryDisplayConfig(uint flags, ref uint paths, [Out] PathInfo[] p, ref uint modes, IntPtr m, IntPtr t);
    [DllImport("user32.dll")] static extern int DisplayConfigGetDeviceInfo(ref ScaleGet info);
    [DllImport("user32.dll")] static extern int DisplayConfigSetDeviceInfo(ref ScaleSet info);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("Shcore.dll")] public static extern int GetProcessDpiAwareness(IntPtr h, out int awareness);
    [DllImport("kernel32.dll")] public static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
    static SourceInfo Source(out uint count) { uint modeCount; int hr = GetDisplayConfigBufferSizes(1, out count, out modeCount); if (hr != 0) throw new Exception("GetDisplayConfigBufferSizes=" + hr); var paths = new PathInfo[count]; IntPtr modes = Marshal.AllocHGlobal((int)modeCount * 64); try { hr = QueryDisplayConfig(1, ref count, paths, ref modeCount, modes, IntPtr.Zero); if (hr != 0) throw new Exception("QueryDisplayConfig=" + hr); return paths[0].sourceInfo; } finally { Marshal.FreeHGlobal(modes); } }
    public static int GetScaleRelative() { uint count; var source = Source(out count); var request = new ScaleGet(); request.header.type = unchecked((uint)-3); request.header.size = (uint)Marshal.SizeOf<ScaleGet>(); request.header.adapterId = source.adapterId; request.header.id = source.id; int hr = DisplayConfigGetDeviceInfo(ref request); if (hr != 0) throw new Exception("DisplayConfigGetDeviceInfo=" + hr); return request.curScaleRel; }
    public static void SetScaleRelative(int relative) { uint count; var source = Source(out count); var request = new ScaleSet(); request.header.type = unchecked((uint)-4); request.header.size = (uint)Marshal.SizeOf<ScaleSet>(); request.header.adapterId = source.adapterId; request.header.id = source.id; request.scaleRel = relative; int hr = DisplayConfigSetDeviceInfo(ref request); if (hr != 0) throw new Exception("DisplayConfigSetDeviceInfo=" + hr); }
}
"@

$manifest = [ordered]@{
    version = 3
    expires_after = "8h"
    idle_timeout = "30m"
    resources = [ordered]@{
        apps = @(
            [ordered]@{
                executable = [System.IO.Path]::GetFullPath($fixtureExe)
                windows = "all"
                terminate = "deny"
            }
        )
    }
    allow = [ordered]@{
        tools = @("start_session", "list_windows", "get_window_state", "click", "type_text", "set_value", "press_key", "scroll", "end_session")
    }
}
$manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $ManifestPath -Encoding UTF8

$socket = "\\.\pipe\cortex-dpi-matrix-$([guid]::NewGuid().ToString('N'))"
$tempRoot = New-Item -ItemType Directory -Path (Join-Path ([System.IO.Path]::GetTempPath()) "cortex-dpi-matrix-$([guid]::NewGuid().ToString('N'))")
$daemon = $null
$fixture = $null

function Invoke-Cua {
    param([string]$Tool, [hashtable]$Arguments)
    $json = $Arguments | ConvertTo-Json -Compress -Depth 8
    $previousErrorAction = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $raw = $json | & $driverExe call --socket $socket $Tool 2>&1 | Out-String
    $exitCode = $LASTEXITCODE
    $ErrorActionPreference = $previousErrorAction
    try { return $raw | ConvertFrom-Json }
    catch { throw "Cua $Tool failed with exit code ${exitCode}: $raw" }
}

function Get-FullState {
    param([hashtable]$Arguments)
    $full = [ordered]@{}
    foreach ($key in $Arguments.Keys) { $full[$key] = $Arguments[$key] }
    $full["since"] = $null
    return Invoke-Cua -Tool "get_window_state" -Arguments $full
}

function Wait-State {
    param([hashtable]$Arguments, [scriptblock]$Predicate)
    $deadline = [DateTime]::UtcNow.AddSeconds(12)
    $last = $null
    do {
        $last = Get-FullState -Arguments $Arguments
        if (& $Predicate $last) { return $last }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $deadline)
    return $last
}

$originalScale = [CortexDpiMatrixNative]::GetScaleRelative()
$results = @()
$baseBounds = $null

try {
    $oldEnvironment = @{}
    foreach ($name in @(
        "CUA_DRIVER_RS_HOME",
        "CUA_DRIVER_RS_TELEMETRY_ENABLED",
        "CUA_TELEMETRY_ENABLED",
        "CUA_TELEMETRY",
        "CUA_TELEMETRY_FORBID_NETWORK",
        "DO_NOT_TRACK"
    )) {
        $oldEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
        [Environment]::SetEnvironmentVariable($name, $null, "Process")
    }
    [Environment]::SetEnvironmentVariable("CUA_DRIVER_RS_HOME", "$($tempRoot.FullName)\driver-home", "Process")
    [Environment]::SetEnvironmentVariable("CUA_DRIVER_RS_TELEMETRY_ENABLED", "false", "Process")
    [Environment]::SetEnvironmentVariable("CUA_TELEMETRY_ENABLED", "false", "Process")
    [Environment]::SetEnvironmentVariable("CUA_TELEMETRY", "0", "Process")
    [Environment]::SetEnvironmentVariable("CUA_TELEMETRY_FORBID_NETWORK", "1", "Process")
    [Environment]::SetEnvironmentVariable("DO_NOT_TRACK", "1", "Process")
    $daemonStart = [System.Diagnostics.ProcessStartInfo]::new()
    $daemonStart.FileName = $driverExe
    $daemonStart.UseShellExecute = $false
    $daemonStart.CreateNoWindow = $true
    $daemonStart.RedirectStandardOutput = $true
    $daemonStart.RedirectStandardError = $true
    $daemonStart.Arguments = "serve --permission-mode bounded --capability-manifest `"$ManifestPath`" --approve-capability-manifest --socket `"$socket`" --no-overlay"
    foreach ($name in @(
        "CUA_DRIVER_RS_HOME",
        "CUA_DRIVER_RS_TELEMETRY_ENABLED",
        "CUA_TELEMETRY_ENABLED",
        "CUA_TELEMETRY",
        "CUA_TELEMETRY_FORBID_NETWORK",
        "DO_NOT_TRACK"
    )) {
        $daemonStart.EnvironmentVariables[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
    }
    $daemon = [System.Diagnostics.Process]::new()
    $daemon.StartInfo = $daemonStart
    if (-not $daemon.Start()) { throw "Cua daemon process could not start." }
    $null = $daemon.StandardOutput.ReadToEndAsync()
    $null = $daemon.StandardError.ReadToEndAsync()
    $ready = $false
    $previousErrorAction = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    for ($attempt = 0; $attempt -lt 60; $attempt++) {
        if ($daemon.HasExited) { break }
        $status = & $driverExe status --socket $socket 2>&1 | Out-String
        if ($LASTEXITCODE -eq 0 -and $status -match "permission mode: bounded" -and $status -match "approved_at_startup=true" -and $status -match "valid=true") { $ready = $true; break }
        Start-Sleep -Milliseconds 100
    }
    $ErrorActionPreference = $previousErrorAction
    if (-not $ready) { throw "Bounded Cua daemon did not become ready." }

    $fixture = Start-Process -FilePath $fixtureExe -PassThru -WindowStyle Normal
    Start-Sleep -Milliseconds 900
    $fixture.Refresh()
    $handle = [CortexDpiMatrixNative]::OpenProcess(0x1000, $false, $fixture.Id)
    $awareness = 0
    try { $null = [CortexDpiMatrixNative]::GetProcessDpiAwareness($handle, [ref]$awareness) }
    finally { $null = [CortexDpiMatrixNative]::CloseHandle($handle) }
    if ($awareness -ne 2) { throw "Fixture is not Per-Monitor V2 aware; got $awareness." }

    $windows = Invoke-Cua -Tool "list_windows" -Arguments @{ pid = $fixture.Id }
    if (@($windows.windows).Count -ne 1) { throw "Fixture did not expose exactly one window." }
    $windowId = [int]$windows.windows[0].window_id

    foreach ($case in @(
        @{ Relative = 0; Scale = 100 },
        @{ Relative = 1; Scale = 125 },
        @{ Relative = 2; Scale = 150 },
        @{ Relative = 4; Scale = 200 }
    )) {
        [CortexDpiMatrixNative]::SetScaleRelative($case.Relative)
        Start-Sleep -Milliseconds 1800
        $fixture.Refresh()
        $dpi = [CortexDpiMatrixNative]::GetDpiForWindow($fixture.MainWindowHandle)
        $session = "dpi-$($case.Scale)-$([guid]::NewGuid().ToString('N'))"
        $null = Invoke-Cua -Tool "start_session" -Arguments @{ session = $session }
        $stateArgs = [ordered]@{
            pid = $fixture.Id
            window_id = $windowId
            tree_format = "elements"
            include_screenshot = $true
            session = $session
        }
        $observed = Get-FullState -Arguments $stateArgs
        $input = $observed.elements | Where-Object { $_.role -eq "Edit" -and $_.label -eq "Message input" } | Select-Object -First 1
        if ($null -eq $input) { throw "Message input was not found at $($case.Scale)%." }
        $text = "Cortex DPI $($case.Scale) pixel smoke"
        $set = Invoke-Cua -Tool "set_value" -Arguments @{ session = $session; pid = $fixture.Id; element_token = $input.element_token; value = $text }
        $afterSet = Wait-State -Arguments $stateArgs -Predicate { param($state) @($state.elements | Where-Object { $_.role -eq "Edit" -and $_.label -eq "Message input" -and $_.value -eq $text }).Count -eq 1 }
        $button = $afterSet.elements | Where-Object { $_.role -eq "Button" -and $_.label -eq "Copy message" } | Select-Object -First 1
        if ($null -eq $button) { throw "Copy button was not found at $($case.Scale)%." }
        $bounds = $afterSet.window_bounds
        $x = [int][math]::Round($button.frame.x + $button.frame.w / 2 - $bounds.x)
        $y = [int][math]::Round($button.frame.y + $button.frame.h / 2 - $bounds.y)
        $click = Invoke-Cua -Tool "click" -Arguments @{
            session = $session; scope = "window"; pid = $fixture.Id; window_id = $windowId
            delivery_mode = "background"; capture_id = [string]$afterSet.capture_id
            x = $x; y = $y; button = "left"; count = 1
        }
        $afterClick = Wait-State -Arguments $stateArgs -Predicate { param($state) @($state.elements | Where-Object { $_.role -eq "Edit" -and $_.label -eq $text -and $_.value -eq $text }).Count -eq 1 }
        $status = $afterClick.elements | Where-Object { $_.role -eq "Edit" -and $_.label -eq $text } | Select-Object -First 1
        $verified = $null -ne $status -and $status.value -eq $text
        if ($case.Scale -eq 100) { $baseBounds = $afterClick.window_bounds }
        $expectedDpi = [int](96 * $case.Scale / 100)
        $expectedRatio = $case.Scale / 100.0
        $ratioX = [math]::Round($afterClick.window_bounds.width / $baseBounds.width, 4)
        $ratioY = [math]::Round($afterClick.window_bounds.height / $baseBounds.height, 4)
        $row = [ordered]@{
            scale_percent = $case.Scale
            scale_rel = [CortexDpiMatrixNative]::GetScaleRelative()
            process_dpi_awareness = $awareness
            window_dpi = $dpi
            expected_window_dpi = $expectedDpi
            window_bounds = $afterClick.window_bounds
            screenshot_width = $afterClick.screenshot_width
            screenshot_height = $afterClick.screenshot_height
            geometry_ratio_x = $ratioX
            geometry_ratio_y = $ratioY
            set_effect = $set.effect
            click_effect = $click.effect
            click_local_x = $x
            click_local_y = $y
            verification_passed = $verified
            status_value = $status.value
        }
        $results += [pscustomobject]$row
        if ($dpi -ne $expectedDpi -or -not $verified -or [math]::Abs($ratioX - $expectedRatio) -gt 0.02 -or [math]::Abs($ratioY - $expectedRatio) -gt 0.02) {
            throw "DPI $($case.Scale)% verification failed: $($row | ConvertTo-Json -Compress)"
        }
        $null = Invoke-Cua -Tool "end_session" -Arguments @{ session = $session }
    }
}
finally {
    [CortexDpiMatrixNative]::SetScaleRelative($originalScale)
    foreach ($entry in $oldEnvironment.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
    }
    if ($null -ne $fixture -and -not $fixture.HasExited) { Stop-Process -Id $fixture.Id -Force -ErrorAction SilentlyContinue }
    if ($null -ne $daemon -and -not $daemon.HasExited) { Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue }
    if ($null -ne $tempRoot -and (Test-Path -LiteralPath $tempRoot.FullName)) { Remove-Item -LiteralPath $tempRoot.FullName -Recurse -Force -ErrorAction SilentlyContinue }
}

$outputDirectory = Split-Path -Parent $OutputPath
$null = New-Item -ItemType Directory -Path $outputDirectory -Force
@{
    schemaVersion = 1
    suite = "cortex-computer-use-windows-dpi-permonitorv2"
    generatedAt = [DateTimeOffset]::UtcNow.ToString("o")
    restoredScaleRelative = $originalScale
    results = $results
} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding UTF8
Write-Output "DPI matrix PASS: $OutputPath"
