param(
    [string[]]$Task = @(),
    [string]$OutputPath = "",
    [switch]$NoBuild,
    [string]$InternalTask = "",
    [string]$WorkerOutput = "",
    [string]$WorkerGate = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Windows.Forms

$script:FixtureRoot = [System.IO.Path]::GetFullPath(
    (Split-Path -Parent $MyInvocation.MyCommand.Path)
)
$script:FixtureExe = Join-Path $script:FixtureRoot "target\cortex-computer-use-windows-fixture.exe"
$script:ChromeHtml = Join-Path $script:FixtureRoot "chrome\acceptance.html"
$script:ArtifactRoot = Join-Path $script:FixtureRoot "artifacts"
$script:ResultRoot = Join-Path $script:FixtureRoot "results"
$script:ScriptPath = [System.IO.Path]::GetFullPath($MyInvocation.MyCommand.Path)
$script:PendingNotepadTestTabRuntimeId = $null

function Resolve-ExecutablePath {
    param([string[]]$Candidates)

    foreach ($candidate in $Candidates) {
        if ([string]::IsNullOrWhiteSpace($candidate)) {
            continue
        }
        try {
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                return [System.IO.Path]::GetFullPath($candidate)
            }
        }
        catch {
        }
    }
    return $null
}

$script:ChromeExe = Resolve-ExecutablePath @(
    "C:\Program Files\Google\Chrome\Application\chrome.exe",
    "C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe"
)
$script:NotepadLauncherExe = Resolve-ExecutablePath @(
    "$env:WINDIR\System32\notepad.exe",
    "$env:WINDIR\notepad.exe"
)
$script:NotepadPackage = $null
$script:NotepadExe = $null
try {
    $script:NotepadPackage = Get-AppxPackage -Name "Microsoft.WindowsNotepad" -ErrorAction Stop |
        Sort-Object Version -Descending |
        Select-Object -First 1
    if ($null -ne $script:NotepadPackage) {
        $script:NotepadExe = Resolve-ExecutablePath @(
            (Join-Path $script:NotepadPackage.InstallLocation "Notepad\Notepad.exe")
        )
    }
}
catch {
}
if ($null -eq $script:NotepadExe) {
    $script:NotepadExe = $script:NotepadLauncherExe
}
$script:NotepadAllowlist = @(
    $script:NotepadLauncherExe
    $script:NotepadExe
) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique
$script:ExplorerExe = Resolve-ExecutablePath @(
    "$env:WINDIR\explorer.exe"
)
$script:ExcelExe = Resolve-ExecutablePath @(
    "C:\Program Files\Microsoft Office\root\Office16\EXCEL.EXE",
    "C:\Program Files (x86)\Microsoft Office\root\Office16\EXCEL.EXE",
    "$env:ProgramFiles\Microsoft Office\root\Office16\EXCEL.EXE"
)

function New-TaskDefinition {
    param(
        [string]$Id,
        [string]$Family,
        [string]$Application,
        [string[]]$Allowlist,
        [int]$TimeoutSeconds
    )

    return [pscustomobject]@{
        Id = $Id
        Family = $Family
        Application = $Application
        Allowlist = @($Allowlist | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        TimeoutSeconds = $TimeoutSeconds
    }
}

$script:Tasks = @(
    New-TaskDefinition "fixture-window-contract" "fixture" "Cortex fixture" @($script:FixtureExe) 20
    New-TaskDefinition "fixture-value-commit" "fixture" "Cortex fixture" @($script:FixtureExe) 20
    New-TaskDefinition "fixture-long-input" "fixture" "Cortex fixture" @($script:FixtureExe) 25
    New-TaskDefinition "fixture-secure-field" "fixture" "Cortex fixture" @($script:FixtureExe) 20
    New-TaskDefinition "fixture-readonly-status" "fixture" "Cortex fixture" @($script:FixtureExe) 20
    New-TaskDefinition "fixture-timeout-exit" "fixture" "Cortex fixture" @($script:FixtureExe) 15
    New-TaskDefinition "fixture-invalid-args" "fixture" "Cortex fixture" @($script:FixtureExe) 15

    New-TaskDefinition "chrome-launch-observe" "chrome" "Google Chrome" @($script:ChromeExe) 45
    New-TaskDefinition "chrome-input-commit" "chrome" "Google Chrome" @($script:ChromeExe) 45
    New-TaskDefinition "chrome-checkbox-toggle" "chrome" "Google Chrome" @($script:ChromeExe) 45
    New-TaskDefinition "chrome-secure-field" "chrome" "Google Chrome" @($script:ChromeExe) 45
    New-TaskDefinition "chrome-link-navigation" "chrome" "Google Chrome" @($script:ChromeExe) 45

    New-TaskDefinition "notepad-open-read" "notepad" "Windows Notepad" @($script:NotepadAllowlist) 40
    New-TaskDefinition "notepad-replace-text" "notepad" "Windows Notepad" @($script:NotepadAllowlist) 40
    New-TaskDefinition "notepad-save-file" "notepad" "Windows Notepad" @($script:NotepadAllowlist) 45

    New-TaskDefinition "explorer-list-folder" "explorer" "Windows Explorer" @($script:ExplorerExe) 40
    New-TaskDefinition "explorer-select-file" "explorer" "Windows Explorer" @($script:ExplorerExe) 40

    New-TaskDefinition "excel-open-workbook" "excel" "Microsoft Excel" @($script:ExcelExe) 75
    New-TaskDefinition "excel-edit-save-cell" "excel" "Microsoft Excel" @($script:ExcelExe) 90

    New-TaskDefinition "multi-monitor-negative-origin" "resilience" "Cortex fixture" @($script:FixtureExe) 30
)

if ($script:Tasks.Count -ne 20) {
    throw "Computer Use acceptance suite must define exactly 20 tasks; got $($script:Tasks.Count)."
}
$families = @($script:Tasks | ForEach-Object { $_.Family } | Sort-Object -Unique)
if ($families.Count -ne 6) {
    throw "Computer Use acceptance suite must define exactly 6 families; got $($families -join ', ')."
}

if (-not ("CortexComputerUseE2E.NativeMethods" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

namespace CortexComputerUseE2E
{
    [StructLayout(LayoutKind.Sequential)]
    public struct JOBOBJECT_BASIC_LIMIT_INFORMATION
    {
        public long PerProcessUserTimeLimit;
        public long PerJobUserTimeLimit;
        public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize;
        public UIntPtr MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint PriorityClass;
        public uint SchedulingClass;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct IO_COUNTERS
    {
        public ulong ReadOperationCount;
        public ulong WriteOperationCount;
        public ulong OtherOperationCount;
        public ulong ReadTransferCount;
        public ulong WriteTransferCount;
        public ulong OtherTransferCount;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION
    {
        public JOBOBJECT_BASIC_LIMIT_INFORMATION BasicLimitInformation;
        public IO_COUNTERS IoInfo;
        public UIntPtr ProcessMemoryLimit;
        public UIntPtr JobMemoryLimit;
        public UIntPtr PeakProcessMemoryUsed;
        public UIntPtr PeakJobMemoryUsed;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct MOUSEINPUT
    {
        public int dx;
        public int dy;
        public uint mouseData;
        public uint dwFlags;
        public uint time;
        public UIntPtr dwExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct KEYBDINPUT
    {
        public ushort wVk;
        public ushort wScan;
        public uint dwFlags;
        public uint time;
        public UIntPtr dwExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct HARDWAREINPUT
    {
        public uint uMsg;
        public ushort wParamL;
        public ushort wParamH;
    }

    [StructLayout(LayoutKind.Explicit)]
    public struct INPUT_UNION
    {
        [FieldOffset(0)]
        public MOUSEINPUT mi;
        [FieldOffset(0)]
        public KEYBDINPUT ki;
        [FieldOffset(0)]
        public HARDWAREINPUT hi;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct INPUT
    {
        public uint type;
        public INPUT_UNION input;
    }

    public static class NativeMethods
    {
        public const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000;
        public const int JobObjectExtendedLimitInformation = 9;
        public const uint INPUT_KEYBOARD = 1;
        public const uint KEYEVENTF_KEYUP = 0x0002;

        public delegate bool EnumWindowsProc(IntPtr window, IntPtr parameter);
        public delegate bool EnumChildWindowsProc(IntPtr window, IntPtr parameter);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern IntPtr CreateJobObject(IntPtr attributes, string name);

        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern bool SetInformationJobObject(
            IntPtr job,
            int informationClass,
            IntPtr information,
            uint informationLength);

        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);

        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern bool CloseHandle(IntPtr handle);

        [DllImport("user32.dll")]
        public static extern bool SetForegroundWindow(IntPtr window);

        [DllImport("user32.dll")]
        public static extern IntPtr GetForegroundWindow();

        [DllImport("user32.dll")]
        public static extern bool EnumWindows(
            EnumWindowsProc callback,
            IntPtr parameter);

        [DllImport("user32.dll")]
        public static extern bool EnumChildWindows(
            IntPtr parent,
            EnumChildWindowsProc callback,
            IntPtr parameter);

        [DllImport("user32.dll")]
        public static extern bool IsWindow(IntPtr window);

        [DllImport("user32.dll")]
        public static extern bool IsWindowVisible(IntPtr window);

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        public static extern int GetWindowTextLength(IntPtr window);

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        public static extern int GetWindowText(
            IntPtr window,
            StringBuilder text,
            int maximumCount);

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern int GetClassName(
            IntPtr window,
            StringBuilder text,
            int maximumCount);

        [DllImport("user32.dll")]
        public static extern int GetDlgCtrlID(IntPtr window);

        [DllImport("user32.dll")]
        public static extern bool IsWindowEnabled(IntPtr window);

        [DllImport("user32.dll")]
        public static extern IntPtr GetParent(IntPtr window);

        [DllImport("user32.dll")]
        public static extern IntPtr GetAncestor(IntPtr window, uint flags);

        [DllImport("user32.dll")]
        public static extern bool IsChild(IntPtr parent, IntPtr child);

        [DllImport("user32.dll", SetLastError = true)]
        public static extern bool PostMessage(
            IntPtr window,
            uint message,
            IntPtr wParam,
            IntPtr lParam);

        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern bool SetWindowText(
            IntPtr window,
            string text);

        [DllImport("user32.dll")]
        private static extern IntPtr SendMessage(
            IntPtr window,
            uint message,
            IntPtr wParam,
            IntPtr lParam);

        public static IntPtr SendWindowMessage(
            IntPtr window,
            uint message,
            IntPtr wParam,
            IntPtr lParam)
        {
            return SendMessage(window, message, wParam, lParam);
        }

        [DllImport("user32.dll")]
        public static extern uint GetWindowThreadProcessId(
            IntPtr window,
            out uint processId);

        [DllImport("user32.dll")]
        public static extern bool ShowWindowAsync(IntPtr window, int command);

        [DllImport("user32.dll")]
        public static extern bool BringWindowToTop(IntPtr window);

        [DllImport("user32.dll")]
        public static extern IntPtr SetFocus(IntPtr window);

        [DllImport("user32.dll")]
        public static extern bool AttachThreadInput(
            uint attachThread,
            uint attachToThread,
            bool attach);

        [DllImport("kernel32.dll")]
        public static extern uint GetCurrentThreadId();

        [DllImport("user32.dll", SetLastError = true)]
        public static extern bool SetWindowPos(
            IntPtr window,
            IntPtr insertAfter,
            int x,
            int y,
            int width,
            int height,
            uint flags);

        [DllImport("user32.dll")]
        public static extern uint GetDpiForWindow(IntPtr window);

        [DllImport("user32.dll", SetLastError = true)]
        private static extern uint SendInput(
            uint inputCount,
            INPUT[] inputs,
            int inputSize);

        public static IntPtr[] GetTopLevelWindows(uint processId, bool visibleOnly)
        {
            List<IntPtr> windows = new List<IntPtr>();
            EnumWindows(delegate(IntPtr window, IntPtr parameter)
            {
                uint ownerProcessId;
                GetWindowThreadProcessId(window, out ownerProcessId);
                if (ownerProcessId != processId)
                {
                    return true;
                }
                if (visibleOnly &&
                    (!IsWindowVisible(window) || GetWindowTextLength(window) == 0))
                {
                    return true;
                }
                windows.Add(window);
                return true;
            }, IntPtr.Zero);
            return windows.ToArray();
        }

        public static IntPtr[] GetChildWindows(IntPtr parent)
        {
            List<IntPtr> windows = new List<IntPtr>();
            EnumChildWindows(parent, delegate(IntPtr window, IntPtr parameter)
            {
                windows.Add(window);
                return true;
            }, IntPtr.Zero);
            return windows.ToArray();
        }

        public static string GetWindowClassName(IntPtr window)
        {
            StringBuilder className = new StringBuilder(256);
            int length = GetClassName(window, className, className.Capacity);
            return length > 0 ? className.ToString() : String.Empty;
        }

        public static void ClickWindowButton(IntPtr window)
        {
            SendMessage(window, 0x00F5, IntPtr.Zero, IntPtr.Zero);
        }

        public static void ClickDialogButton(
            IntPtr dialog,
            int controlId,
            IntPtr button)
        {
            SendMessage(
                dialog,
                0x0111,
                new IntPtr(controlId),
                button);
        }

        public static string GetTopLevelWindowTitle(IntPtr window)
        {
            int length = GetWindowTextLength(window);
            if (length <= 0)
            {
                return String.Empty;
            }
            StringBuilder title = new StringBuilder(length + 1);
            GetWindowText(window, title, title.Capacity);
            return title.ToString();
        }

        public static bool ContainsToken(
            byte[] data,
            string token,
            bool unicode,
            bool requireNullBoundary)
        {
            byte[] needle = (unicode ? Encoding.Unicode : Encoding.ASCII).GetBytes(token);
            int width = unicode ? 2 : 1;
            for (int index = 0; index <= data.Length - needle.Length; index++)
            {
                bool match = true;
                for (int offset = 0; offset < needle.Length; offset++)
                {
                    if (data[index + offset] != needle[offset])
                    {
                        match = false;
                        break;
                    }
                }
                if (!match)
                {
                    continue;
                }
                if (!requireNullBoundary)
                {
                    return true;
                }
                bool startsAtBoundary =
                    index < width ||
                    (data[index - width] == 0 &&
                     (!unicode || data[index - width + 1] == 0));
                int after = index + needle.Length;
                bool endsAtBoundary =
                    after >= data.Length ||
                    (data[after] == 0 &&
                     (!unicode ||
                      (after + 1 < data.Length && data[after + 1] == 0)));
                if (startsAtBoundary && endsAtBoundary)
                {
                    return true;
                }
            }
            return false;
        }

        public static void SendVirtualKey(ushort virtualKey)
        {
            List<INPUT> inputs = new List<INPUT>();
            inputs.Add(CreateKeyboardInput(virtualKey, 0, 0));
            inputs.Add(CreateKeyboardInput(virtualKey, 0, KEYEVENTF_KEYUP));
            SendKeyboardInputs(inputs);
        }

        public static void SendControlShortcut(ushort virtualKey)
        {
            List<INPUT> inputs = new List<INPUT>();
            inputs.Add(CreateKeyboardInput(0x11, 0, 0));
            inputs.Add(CreateKeyboardInput(virtualKey, 0, 0));
            inputs.Add(CreateKeyboardInput(virtualKey, 0, KEYEVENTF_KEYUP));
            inputs.Add(CreateKeyboardInput(0x11, 0, KEYEVENTF_KEYUP));
            SendKeyboardInputs(inputs);
        }

        private static INPUT CreateKeyboardInput(
            ushort virtualKey,
            ushort scanCode,
            uint flags)
        {
            INPUT input = new INPUT();
            input.type = INPUT_KEYBOARD;
            input.input.ki.wVk = virtualKey;
            input.input.ki.wScan = scanCode;
            input.input.ki.dwFlags = flags;
            input.input.ki.time = 0;
            input.input.ki.dwExtraInfo = UIntPtr.Zero;
            return input;
        }

        private static void SendKeyboardInputs(List<INPUT> inputs)
        {
            if (inputs.Count == 0)
            {
                return;
            }
            INPUT[] inputArray = inputs.ToArray();
            int inputSize = Marshal.SizeOf(typeof(INPUT));
            uint sent = SendInput(
                (uint)inputArray.Length,
                inputArray,
                inputSize);
            if (sent != (uint)inputArray.Length)
            {
                throw new System.ComponentModel.Win32Exception(
                    Marshal.GetLastWin32Error(),
                    "SendInput could not deliver every keyboard event.");
            }
        }
    }
}
"@
}

function Wait-Until {
    param(
        [scriptblock]$Condition,
        [int]$TimeoutMilliseconds,
        [string]$FailureMessage,
        [int]$PollMilliseconds = 100
    )

    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    while ($stopwatch.ElapsedMilliseconds -lt $TimeoutMilliseconds) {
        try {
            $conditionResult = & $Condition
            if ($conditionResult) {
                return $conditionResult
            }
        }
        catch {
        }
        Start-Sleep -Milliseconds $PollMilliseconds
    }
    throw $FailureMessage
}

function New-TestDirectory {
    param([string]$Purpose)

    $null = New-Item -ItemType Directory -Path $script:ArtifactRoot -Force
    $safePurpose = $Purpose -replace "[^A-Za-z0-9_.-]", "-"
    $path = Join-Path $script:ArtifactRoot (
        "{0}-{1}" -f $safePurpose, [guid]::NewGuid().ToString("N")
    )
    $null = New-Item -ItemType Directory -Path $path
    return [System.IO.Path]::GetFullPath($path)
}

function Remove-TestDirectory {
    param([string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path) -or -not (Test-Path -LiteralPath $Path)) {
        return
    }
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $artifactRoot = [System.IO.Path]::GetFullPath($script:ArtifactRoot)
    $rootPrefix = $artifactRoot.TrimEnd('\') + '\'
    if (-not $fullPath.StartsWith(
            $rootPrefix,
            [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove a test directory outside the fixture artifact root: $fullPath"
    }
    for ($attempt = 1; $attempt -le 10; $attempt++) {
        try {
            Remove-Item -LiteralPath $fullPath -Recurse -Force -ErrorAction Stop
            return
        }
        catch {
            if ($attempt -eq 10) {
                throw
            }
            Start-Sleep -Milliseconds 500
        }
    }
}

function Start-ExactProcess {
    param(
        [string]$FilePath,
        [string]$Arguments = "",
        [string]$WorkingDirectory = ""
    )

    if ([string]::IsNullOrWhiteSpace($FilePath)) {
        throw "An exact executable path is required."
    }
    $fullPath = [System.IO.Path]::GetFullPath($FilePath)
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        throw "Executable does not exist: $fullPath"
    }

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $fullPath
    $startInfo.Arguments = $Arguments
    $startInfo.UseShellExecute = $false
    if (-not [string]::IsNullOrWhiteSpace($WorkingDirectory)) {
        $startInfo.WorkingDirectory = [System.IO.Path]::GetFullPath($WorkingDirectory)
    }

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        throw "Process did not start: $fullPath"
    }
    return $process
}

function Quote-ProcessArgument {
    param([string]$Value)

    if ($null -eq $Value) {
        return '""'
    }
    return '"' + $Value.Replace('"', '\"') + '"'
}

function Get-NormalizedPath {
    param([string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path)) {
        return ""
    }
    return [System.IO.Path]::GetFullPath($Path).TrimEnd('\').ToLowerInvariant()
}

function Assert-ExactProcessPath {
    param(
        [System.Diagnostics.Process]$Process,
        [string[]]$Allowlist
    )

    $actual = $null
    for ($attempt = 1; $attempt -le 20; $attempt++) {
        $Process.Refresh()
        try {
            $actual = $Process.Path
        }
        catch {
            $actual = $null
        }
        if (-not [string]::IsNullOrWhiteSpace($actual)) {
            break
        }
        Start-Sleep -Milliseconds 100
    }
    if ([string]::IsNullOrWhiteSpace($actual)) {
        throw "Cannot prove the executable path for pid $($Process.Id)."
    }
    $actualNormalized = Get-NormalizedPath -Path $actual
    $allowed = @(
        $Allowlist |
            ForEach-Object { Get-NormalizedPath -Path $_ } |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    )
    if (-not ($allowed -contains $actualNormalized)) {
        throw "Process $($Process.Id) executable '$actual' is outside the task allowlist."
    }
    return $actual
}

function Get-ProcessCommandLine {
    param([int]$ProcessId)

    $processInfo = @(
        Get-CimInstance Win32_Process -Filter "ProcessId=$ProcessId" -ErrorAction Stop
    )
    if ($processInfo.Count -ne 1) {
        throw "Could not read exactly one command line for pid $ProcessId."
    }
    return [string]$processInfo[0].CommandLine
}

function Get-WindowHandleHex {
    param([IntPtr]$WindowHandle)

    return "0x{0:X}" -f $WindowHandle.ToInt64()
}

function Convert-WindowHandle {
    param([string]$WindowHandle)

    if ([string]::IsNullOrWhiteSpace($WindowHandle) -or
        -not $WindowHandle.StartsWith("0x", [StringComparison]::OrdinalIgnoreCase)) {
        throw "Invalid hexadecimal window handle: '$WindowHandle'."
    }
    return [IntPtr]::new(
        [Convert]::ToInt64($WindowHandle.Substring(2), 16)
    )
}

function Get-NotepadIsolationContract {
    # Packaged Notepad can redirect an ordinary file launch into its existing
    # AppInstance. A switch name is only a lead; the post-launch exact-HWND
    # proof below remains mandatory before any UI input.
    $candidateSwitches = @("-n", "--new-window")
    $foundSwitches = @()
    if (-not [string]::IsNullOrWhiteSpace($script:NotepadExe) -and
        (Test-Path -LiteralPath $script:NotepadExe -PathType Leaf)) {
        $bytes = [System.IO.File]::ReadAllBytes($script:NotepadExe)
        foreach ($candidate in $candidateSwitches) {
            $foundInUnicode = [CortexComputerUseE2E.NativeMethods]::ContainsToken(
                $bytes,
                $candidate,
                $true,
                $true
            )
            $foundInAscii = [CortexComputerUseE2E.NativeMethods]::ContainsToken(
                $bytes,
                $candidate,
                $false,
                $true
            )
            if ($foundInUnicode -or $foundInAscii) {
                $foundSwitches += $candidate
            }
        }
        $hasAppLifecycle = [CortexComputerUseE2E.NativeMethods]::ContainsToken(
            $bytes,
            "Microsoft.Windows.AppLifecycle.AppInstance",
            $true,
            $false
        )
        $hasSessionSwitch = [CortexComputerUseE2E.NativeMethods]::ContainsToken(
            $bytes,
            "/SESSION:",
            $true,
            $false
        )
    }
    else {
        $hasAppLifecycle = $false
        $hasSessionSwitch = $false
    }

    $packageVersion = if ($null -ne $script:NotepadPackage) {
        [string]$script:NotepadPackage.Version
    }
    else {
        $null
    }
    $aumid = if ($null -ne $script:NotepadPackage) {
        "$($script:NotepadPackage.PackageFamilyName)!App"
    }
    else {
        $null
    }
    return [ordered]@{
        packageName = if ($null -ne $script:NotepadPackage) {
            [string]$script:NotepadPackage.Name
        }
        else {
            $null
        }
        packageVersion = $packageVersion
        aumid = $aumid
        executable = $script:NotepadExe
        candidateSwitches = @($candidateSwitches)
        foundCandidateSwitches = @($foundSwitches)
        packagedAppLifecycleMarkers = [ordered]@{
            appInstance = $hasAppLifecycle
            sessionArgument = $hasSessionSwitch
        }
        stringEncodingsScanned = @("Unicode", "ASCII")
        uiLaunchAttempted = $false
    }
}

function Get-TopLevelWindowsForProcessIds {
    param([int[]]$ProcessIds)

    $windows = @()
    foreach ($processId in @($ProcessIds | Sort-Object -Unique)) {
        if ($processId -le 0) {
            continue
        }
        foreach ($windowHandle in @(
                [CortexComputerUseE2E.NativeMethods]::GetTopLevelWindows(
                    [uint32]$processId,
                    $true
                )
            )) {
            $windows += [ordered]@{
                processId = $processId
                handle = (Get-WindowHandleHex -WindowHandle $windowHandle)
                title = (
                    [CortexComputerUseE2E.NativeMethods]::GetTopLevelWindowTitle(
                        $windowHandle
                    )
                )
            }
        }
    }
    return @($windows)
}

function Get-AppWindowSnapshot {
    param([string[]]$ProcessNames)

    $processes = @(
        foreach ($processName in $ProcessNames) {
            Get-CimInstance Win32_Process `
                -Filter "Name='$processName.exe'" -ErrorAction SilentlyContinue
        }
    )
    $processIds = @(
        $processes |
            ForEach-Object { [int]$_.ProcessId } |
            Sort-Object -Unique
    )
    $windows = @(
        Get-TopLevelWindowsForProcessIds -ProcessIds $processIds |
            Sort-Object {
                [Convert]::ToInt64(([string]$_.handle).Substring(2), 16)
            }
    )
    return [ordered]@{
        processNames = @($ProcessNames)
        processIds = @($processIds)
        windows = $windows
    }
}

function Test-WindowsSessionLocked {
    $foreground = [CortexComputerUseE2E.NativeMethods]::GetForegroundWindow()
    if ($foreground -eq [IntPtr]::Zero) {
        return $true
    }
    $foregroundProcessId = [uint32]0
    $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
        $foreground,
        [ref]$foregroundProcessId
    )
    if ($foregroundProcessId -eq 0) {
        return $true
    }
    $process = Get-Process -Id $foregroundProcessId -ErrorAction SilentlyContinue
    if ($null -eq $process) {
        return $true
    }
    try {
        $processName = [System.IO.Path]::GetFileName([string]$process.Path)
        return ($processName -ieq "LockApp.exe" -or $processName -ieq "LogonUI.exe")
    }
    catch {
        return $true
    }
}

function Assert-AppWindowsPreserved {
    param([object]$Snapshot)

    foreach ($window in @($Snapshot.windows)) {
        $handle = Convert-WindowHandle -WindowHandle ([string]$window.handle)
        if (-not [CortexComputerUseE2E.NativeMethods]::IsWindow($handle)) {
            throw (
                "A pre-existing application window disappeared during the isolated " +
                "task: pid $($window.processId), hwnd $($window.handle)."
            )
        }
        $ownerProcessId = [uint32]0
        $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
            $handle,
            [ref]$ownerProcessId
        )
        if ([int]$ownerProcessId -ne [int]$window.processId) {
            throw (
                "A pre-existing application window changed ownership during the " +
                "isolated task: pid $($window.processId), hwnd $($window.handle)."
            )
        }
        # 浏览器标题会因后台标签页、通知或页面路由自然变化；标题不是窗口
        # 身份。这里仍要求 hwnd 存续且 owner pid 不变，新测试窗口另由
        # baseline hwnd 集合证明它不是用户原有窗口。
    }
    return $true
}

function Assert-ExactTestWindow {
    param(
        [System.Diagnostics.Process]$Process,
        [IntPtr]$WindowHandle,
        [object]$BaselineSnapshot
    )

    if ($WindowHandle -eq [IntPtr]::Zero -or -not
        [CortexComputerUseE2E.NativeMethods]::IsWindow($WindowHandle)) {
        throw "The isolated test window handle is not valid."
    }
    $ownerProcessId = [uint32]0
    $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
        $WindowHandle,
        [ref]$ownerProcessId
    )
    if ([int]$ownerProcessId -ne $Process.Id) {
        throw (
            "The test window belongs to pid $ownerProcessId, not the exact launched " +
            "process pid $($Process.Id)."
        )
    }
    $handleText = Get-WindowHandleHex -WindowHandle $WindowHandle
    $baselineHandles = @(
        $BaselineSnapshot.windows |
            ForEach-Object { [string]$_.handle.ToUpperInvariant() }
    )
    if ($baselineHandles -contains $handleText.ToUpperInvariant()) {
        throw "The isolated test window matched a pre-existing application window."
    }
    $windowsPreserved = Assert-AppWindowsPreserved -Snapshot $BaselineSnapshot
    if (-not $windowsPreserved) {
        throw "Pre-existing application windows were not preserved."
    }
    return [ordered]@{
        processId = $Process.Id
        windowId = $handleText
        windowProcessId = [int]$ownerProcessId
        windowNotInBaseline = $true
        preExistingProcessIds = @($BaselineSnapshot.processIds)
        preExistingWindows = @($BaselineSnapshot.windows)
        preExistingWindowsPreserved = $true
    }
}

function Wait-MainWindow {
    param(
        [System.Diagnostics.Process]$Process,
        [int]$TimeoutMilliseconds,
        [string]$TitlePattern = ""
    )

    $null = Wait-Until -TimeoutMilliseconds $TimeoutMilliseconds -FailureMessage (
        "Process $($Process.Id) did not expose an allowlisted main window within " +
        "$TimeoutMilliseconds ms."
    ) -PollMilliseconds 100 -Condition {
        $Process.Refresh()
        if ($Process.HasExited -or $Process.MainWindowHandle -eq [IntPtr]::Zero) {
            return $false
        }
        if ([string]::IsNullOrWhiteSpace($TitlePattern)) {
            return $true
        }
        return $Process.MainWindowTitle -match $TitlePattern
    }
    return $Process.MainWindowHandle
}

function Get-WindowRoot {
    param(
        [System.Diagnostics.Process]$Process,
        [string[]]$Allowlist
    )

    $null = Assert-ExactProcessPath -Process $Process -Allowlist $Allowlist
    $handle = Wait-MainWindow -Process $Process -TimeoutMilliseconds 15000
    $root = [Windows.Automation.AutomationElement]::FromHandle($handle)
    if ($null -eq $root) {
        throw "UIA could not attach to pid $($Process.Id), hwnd $handle."
    }
    if ($root.Current.ProcessId -ne $Process.Id) {
        throw "UIA root belongs to pid $($root.Current.ProcessId), expected $($Process.Id)."
    }
    return $root
}

function Get-WindowRootAtHandle {
    param(
        [System.Diagnostics.Process]$Process,
        [IntPtr]$WindowHandle,
        [string[]]$Allowlist
    )

    $null = Assert-ExactProcessPath -Process $Process -Allowlist $Allowlist
    if ($WindowHandle -eq [IntPtr]::Zero -or
        -not [CortexComputerUseE2E.NativeMethods]::IsWindow($WindowHandle)) {
        throw "The exact test window handle is not valid."
    }
    $ownerProcessId = [uint32]0
    $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
        $WindowHandle,
        [ref]$ownerProcessId
    )
    if ([int]$ownerProcessId -ne $Process.Id) {
        throw (
            "The exact test window belongs to pid $ownerProcessId, not pid " +
            "$($Process.Id)."
        )
    }
    $root = [Windows.Automation.AutomationElement]::FromHandle($WindowHandle)
    if ($null -eq $root) {
        throw "UIA could not attach to the exact test window $WindowHandle."
    }
    if ($root.Current.ProcessId -ne $Process.Id) {
        throw (
            "UIA root for the exact test window belongs to pid " +
            "$($root.Current.ProcessId), expected pid $($Process.Id)."
        )
    }
    return $root
}

function Find-ByProperty {
    param(
        [Windows.Automation.AutomationElement]$Root,
        [Windows.Automation.AutomationProperty]$Property,
        [object]$Value,
        [Windows.Automation.TreeScope]$Scope =
            [Windows.Automation.TreeScope]::Descendants
    )

    $condition = [Windows.Automation.PropertyCondition]::new($Property, $Value)
    return $Root.FindFirst($Scope, $condition)
}

function Find-ByAutomationId {
    param(
        [Windows.Automation.AutomationElement]$Root,
        [string]$AutomationId
    )

    return Find-ByProperty -Root $Root `
        -Property ([Windows.Automation.AutomationElement]::AutomationIdProperty) `
        -Value $AutomationId
}

function Find-ByName {
    param(
        [Windows.Automation.AutomationElement]$Root,
        [string]$Name
    )

    return Find-ByProperty -Root $Root `
        -Property ([Windows.Automation.AutomationElement]::NameProperty) `
        -Value $Name
}

function Find-ByNameAndControlType {
    param(
        [Windows.Automation.AutomationElement]$Root,
        [string]$Name,
        [Windows.Automation.ControlType]$ControlType
    )

    $nameCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::NameProperty,
        $Name
    )
    $typeCondition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ControlTypeProperty,
        $ControlType
    )
    $condition = [Windows.Automation.AndCondition]::new(
        [Windows.Automation.Condition[]]@($nameCondition, $typeCondition)
    )
    return $Root.FindFirst(
        [Windows.Automation.TreeScope]::Descendants,
        $condition
    )
}

function Get-AllDescendants {
    param([Windows.Automation.AutomationElement]$Root)

    return $Root.FindAll(
        [Windows.Automation.TreeScope]::Descendants,
        [Windows.Automation.Condition]::TrueCondition
    )
}

function Get-ValuePattern {
    param([Windows.Automation.AutomationElement]$Element)

    $pattern = $null
    if (-not $Element.TryGetCurrentPattern(
            [Windows.Automation.ValuePattern]::Pattern,
            [ref]$pattern)) {
        throw "Element '$($Element.Current.Name)' has no ValuePattern."
    }
    return [Windows.Automation.ValuePattern]$pattern
}

function Get-InvokePattern {
    param([Windows.Automation.AutomationElement]$Element)

    $pattern = $null
    if (-not $Element.TryGetCurrentPattern(
            [Windows.Automation.InvokePattern]::Pattern,
            [ref]$pattern)) {
        throw "Element '$($Element.Current.Name)' has no InvokePattern."
    }
    return [Windows.Automation.InvokePattern]$pattern
}

function Get-TogglePattern {
    param([Windows.Automation.AutomationElement]$Element)

    $pattern = $null
    if (-not $Element.TryGetCurrentPattern(
            [Windows.Automation.TogglePattern]::Pattern,
            [ref]$pattern)) {
        throw "Element '$($Element.Current.Name)' has no TogglePattern."
    }
    return [Windows.Automation.TogglePattern]$pattern
}

function Get-SelectionItemPattern {
    param([Windows.Automation.AutomationElement]$Element)

    $pattern = $null
    if (-not $Element.TryGetCurrentPattern(
            [Windows.Automation.SelectionItemPattern]::Pattern,
            [ref]$pattern)) {
        throw "Element '$($Element.Current.Name)' has no SelectionItemPattern."
    }
    return [Windows.Automation.SelectionItemPattern]$pattern
}

function Get-ControlText {
    param([Windows.Automation.AutomationElement]$Element)

    $pattern = $null
    if ($Element.TryGetCurrentPattern(
            [Windows.Automation.TextPattern]::Pattern,
            [ref]$pattern)) {
        try {
            $textPattern = [Windows.Automation.TextPattern]$pattern
            return $textPattern.DocumentRange.GetText(-1)
        }
        catch {
        }
    }

    $pattern = $null
    if ($Element.TryGetCurrentPattern(
            [Windows.Automation.ValuePattern]::Pattern,
            [ref]$pattern)) {
        try {
            return ([Windows.Automation.ValuePattern]$pattern).Current.Value
        }
        catch {
        }
    }
    return $Element.Current.Name
}

function Assert-ForegroundOwnership {
    param(
        [System.Diagnostics.Process]$Process,
        [IntPtr]$WindowHandle
    )

    $targetProcessId = [uint32]0
    $targetThread = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
        $WindowHandle,
        [ref]$targetProcessId
    )
    $currentThread = [CortexComputerUseE2E.NativeMethods]::GetCurrentThreadId()
    $activationDeadline = [DateTime]::UtcNow.AddSeconds(3)
    do {
        $foregroundWindow = [CortexComputerUseE2E.NativeMethods]::GetForegroundWindow()
        # 已在目标顶层窗口时不要重新 SetFocus，否则 Excel 会退出单元格编辑态。
        if ($foregroundWindow -eq $WindowHandle) { break }
        $foregroundProcessId = [uint32]0
        $foregroundThread = if ($foregroundWindow -eq [IntPtr]::Zero) {
            0
        }
        else {
            [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
                $foregroundWindow,
                [ref]$foregroundProcessId
            )
        }
        $attachedForeground = $false
        $attachedTarget = $false
        if ($foregroundThread -ne 0 -and
            $foregroundThread -ne $currentThread) {
            $attachedForeground =
                [CortexComputerUseE2E.NativeMethods]::AttachThreadInput(
                    $currentThread,
                    $foregroundThread,
                    $true
                )
        }
        if ($targetThread -ne 0 -and $targetThread -ne $currentThread) {
            $attachedTarget =
                [CortexComputerUseE2E.NativeMethods]::AttachThreadInput(
                    $currentThread,
                    $targetThread,
                    $true
                )
        }
        try {
            # Join the current foreground input queue before activation.
            try {
                $windowElement = [Windows.Automation.AutomationElement]::FromHandle(
                    $WindowHandle
                )
                $windowElement.SetFocus()
            }
            catch {
            }
            $null = [CortexComputerUseE2E.NativeMethods]::ShowWindowAsync(
                $WindowHandle,
                9
            )
            $null = [CortexComputerUseE2E.NativeMethods]::BringWindowToTop(
                $WindowHandle
            )
            $null = [CortexComputerUseE2E.NativeMethods]::SetForegroundWindow(
                $WindowHandle
            )
            $null = [CortexComputerUseE2E.NativeMethods]::SetFocus($WindowHandle)
        }
        finally {
            if ($attachedTarget) {
                $null = [CortexComputerUseE2E.NativeMethods]::AttachThreadInput(
                    $currentThread,
                    $targetThread,
                    $false
                )
            }
            if ($attachedForeground) {
                $null = [CortexComputerUseE2E.NativeMethods]::AttachThreadInput(
                    $currentThread,
                    $foregroundThread,
                    $false
                )
            }
        }
        if ([CortexComputerUseE2E.NativeMethods]::GetForegroundWindow() -eq
            $WindowHandle) {
            break
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $activationDeadline)

    $actualForeground = [CortexComputerUseE2E.NativeMethods]::GetForegroundWindow()
    if ($actualForeground -ne $WindowHandle) {
        $actualForegroundProcessId = [uint32]0
        if ($actualForeground -ne [IntPtr]::Zero) {
            $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
                $actualForeground,
                [ref]$actualForegroundProcessId
            )
        }
        throw (
            "Could not prove foreground ownership for pid $($Process.Id), " +
            "hwnd $(Get-WindowHandleHex -WindowHandle $WindowHandle); actual " +
            "foreground was pid $actualForegroundProcessId, hwnd " +
            "$(Get-WindowHandleHex -WindowHandle $actualForeground)."
        )
    }
    $Process.Refresh()
    if ($Process.HasExited -or
        -not [CortexComputerUseE2E.NativeMethods]::IsWindow($WindowHandle)) {
        throw "The exact test process or window became invalid during keyboard input."
    }
    $ownerProcessId = [uint32]0
    $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
        $WindowHandle,
        [ref]$ownerProcessId
    )
    if ([int]$ownerProcessId -ne $Process.Id) {
        throw (
            "Foreground ownership changed to pid $ownerProcessId; expected the " +
            "exact test process pid $($Process.Id)."
        )
    }
}

function Send-KeysToOwnedWindow {
    param(
        [System.Diagnostics.Process]$Process,
        [IntPtr]$WindowHandle,
        [string]$Keys
    )

    # GetForegroundWindow 只返回顶层窗口；编辑 HWND 必须通过父子关系和焦点另行证明。
    $topLevel = [CortexComputerUseE2E.NativeMethods]::GetAncestor($WindowHandle, 2)
    if ($topLevel -eq [IntPtr]::Zero) { throw 'Keyboard target has no top-level window.' }
    Assert-ForegroundOwnership -Process $Process -WindowHandle $topLevel
    if ($topLevel -ne $WindowHandle) {
        [uint32]$controlProcessId = 0
        $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
            $WindowHandle, [ref]$controlProcessId
        )
        $focused = [Windows.Automation.AutomationElement]::FocusedElement
        if ($controlProcessId -ne $Process.Id -or
            -not [CortexComputerUseE2E.NativeMethods]::IsChild($topLevel, $WindowHandle) -or
            $focused.Current.ProcessId -ne $Process.Id -or
            [long]$focused.Current.NativeWindowHandle -ne $WindowHandle.ToInt64()) {
            throw 'The exact child edit control does not own keyboard focus.'
        }
    }
    [System.Windows.Forms.SendKeys]::SendWait($Keys)
}

function Stop-TestProcess {
    param([System.Diagnostics.Process]$Process)

    if ($null -eq $Process) {
        return "no process started"
    }
    try {
        $Process.Refresh()
        if ($Process.HasExited) {
            return "already exited"
        }

        $null = $Process.CloseMainWindow()
        if ($Process.WaitForExit(1500)) {
            return "closed main window"
        }
    Start-Sleep -Milliseconds 1500
    $Process.Refresh()
    if ($Process.HasExited) {
        return "closed main window after delayed shutdown"
    }

        $taskkill = Join-Path $env:WINDIR "System32\taskkill.exe"
        $killInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $killInfo.FileName = $taskkill
        $killInfo.Arguments = "/PID $($Process.Id) /T /F"
        $killInfo.UseShellExecute = $false
        $killInfo.CreateNoWindow = $true
        $killer = [System.Diagnostics.Process]::Start($killInfo)
        $null = $killer.WaitForExit(5000)
        if (-not $Process.WaitForExit(5000)) {
            $Process.Refresh()
            if (-not $Process.HasExited) {
                throw "Timed-out process $($Process.Id) could not be cleaned up."
            }
        }
        return "terminated exact process tree after close timeout"
    }
    finally {
        $Process.Dispose()
    }
}

function New-PassResult {
    param(
        [string]$Kind,
        [object]$Expected,
        [object]$Actual,
        [object]$Details = $null
    )

    return [ordered]@{
        status = "pass"
        reason = $null
        verifier = [ordered]@{
            kind = $Kind
            expected = $Expected
            actual = $Actual
        }
        details = $Details
        cleanup = @()
    }
}

function New-SkipResult {
    param(
        [string]$Reason,
        [object]$Details = $null
    )

    return [ordered]@{
        status = "skipped"
        reason = $Reason
        verifier = $null
        details = $Details
        cleanup = @()
    }
}

function Add-CleanupEvidence {
    param(
        [object]$Result,
        [string]$Evidence
    )

    if ($null -eq $Result) {
        return
    }
    $Result.cleanup = @($Result.cleanup) + $Evidence
}

function Start-Fixture {
    param([string]$Arguments = "")

    return Start-ExactProcess -FilePath $script:FixtureExe -Arguments $Arguments
}

function Wait-FixtureElements {
    param(
        [System.Diagnostics.Process]$Process,
        [string[]]$Allowlist
    )

    $null = Assert-ExactProcessPath -Process $Process -Allowlist $Allowlist

    # `Process.MainWindowHandle` 只是 Win32 的启发式结果，测试进程短暂创建
    # 的消息窗口或弹层时可能指向一个没有 AutomationId 的顶层窗口。这里按
    # 精确 PID 枚举，只接受标题和稳定 AutomationId 同时匹配的 fixture 根。
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    $root = $null
    $observed = [System.Collections.Generic.List[string]]::new()
    while ([DateTime]::UtcNow -lt $deadline -and $null -eq $root) {
        $Process.Refresh()
        if ($Process.HasExited) {
            break
        }
        foreach ($handle in @(
                [CortexComputerUseE2E.NativeMethods]::GetTopLevelWindows(
                    [uint32]$Process.Id,
                    $true
                )
            )) {
            $title = [CortexComputerUseE2E.NativeMethods]::GetTopLevelWindowTitle($handle)
            if ($title -ne "Cortex Computer Use Fixture") {
                continue
            }
            $candidate = [Windows.Automation.AutomationElement]::FromHandle($handle)
            if ($null -eq $candidate -or $candidate.Current.ProcessId -ne $Process.Id) {
                continue
            }
            $automationId = $candidate.Current.AutomationId
            if ($observed.Count -lt 12) {
                $observed.Add(
                    "$(Get-WindowHandleHex -WindowHandle $handle) title='$title' automationId='$automationId'"
                )
            }
            if ($automationId -eq "computer-use-fixture-window") {
                $root = $candidate
                break
            }
        }
        if ($null -eq $root) {
            Start-Sleep -Milliseconds 100
        }
    }
    if ($null -eq $root) {
        throw (
            "No exact fixture window exposed title 'Cortex Computer Use Fixture' " +
            "and AutomationId 'computer-use-fixture-window'. Observed: " +
            ($observed -join "; ")
        )
    }
    $null = Wait-Until -TimeoutMilliseconds 8000 -FailureMessage (
        "The fixture window existed but its stable UIA controls were not ready."
    ) -Condition {
        foreach ($id in @("1001", "1002", "1003", "1004")) {
            if ($null -eq (Find-ByAutomationId -Root $root -AutomationId $id)) {
                return $false
            }
        }
        return $true
    }
    return $root
}

function Invoke-FixtureAction {
    param(
        [string]$TaskId,
        [string[]]$Allowlist
    )

    if ($TaskId -eq "fixture-timeout-exit") {
        $process = Start-Fixture -Arguments "--status timeout-check --exit-timeout-ms 2000"
        $cleanup = @()
        try {
            $null = Assert-ExactProcessPath -Process $process -Allowlist $Allowlist
            $handle = Wait-MainWindow -Process $process -TimeoutMilliseconds 5000
            $root = [Windows.Automation.AutomationElement]::FromHandle($handle)
            if ($null -eq $root -or $root.Current.ProcessId -ne $process.Id) {
                throw "Fixture timed window was not owned by the exact test process."
            }
            if (-not $process.WaitForExit(5000)) {
                throw "Fixture did not honor --exit-timeout-ms=2000."
            }
            if ($process.ExitCode -ne 0) {
                throw "Timed fixture exited with code $($process.ExitCode), expected 0."
            }
            return New-PassResult -Kind "process exit code and window lifecycle" `
                -Expected "window present, then exit code 0 within 5000 ms" `
                -Actual "window '$($root.Current.Name)' present; exit code $($process.ExitCode)" `
                -Details @{ pid = $process.Id; exitCode = $process.ExitCode }
        }
        finally {
            $cleanup += Stop-TestProcess -Process $process
        }
    }

    if ($TaskId -eq "fixture-invalid-args") {
        $process = Start-Fixture -Arguments "--exit-timeout-ms 0"
        try {
            $mainWindowCreated = $false
            $observedWindows = @()
            $windowPollDeadline = [DateTime]::UtcNow.AddSeconds(3)
            while (-not $process.HasExited -and [DateTime]::UtcNow -lt $windowPollDeadline) {
                $process.Refresh()
                # Process.MainWindowHandle is a heuristic and can transiently point
                # at a hidden or unrelated window. Contract that no visible titled
                # top-level window exists by enumerating the exact PID.
                $observedWindows = @(
                    Get-TopLevelWindowsForProcessIds -ProcessIds @($process.Id)
                )
                if ($observedWindows.Count -gt 0) {
                    $mainWindowCreated = $true
                    break
                }
                Start-Sleep -Milliseconds 20
            }
            if (-not $process.WaitForExit(3000)) {
                throw "Fixture accepted invalid arguments and remained running."
            }
            if ($process.ExitCode -ne 2) {
                throw "Invalid fixture arguments exited with $($process.ExitCode), expected 2."
            }
            if ($mainWindowCreated) {
                throw (
                    "Invalid fixture arguments created a visible top-level window " +
                    "before exiting: " + ($observedWindows | ConvertTo-Json -Compress)
                )
            }
            return New-PassResult -Kind "process exit code and observed window lifecycle" `
                -Expected "no main window; exit code 2" `
                -Actual "no main window observed; exit code $($process.ExitCode)" `
                -Details @{
                    mainWindowCreated = $mainWindowCreated
                    observedWindows = @($observedWindows)
                    pid = $process.Id
                }
        }
        finally {
            $null = Stop-TestProcess -Process $process
        }
    }

    $initialStatus = "fixture-$TaskId-$([guid]::NewGuid().ToString('N'))"
    $process = Start-Fixture `
        -Arguments "--status $(Quote-ProcessArgument $initialStatus) --exit-timeout-ms 30000"
    try {
        $root = Wait-FixtureElements -Process $process -Allowlist $Allowlist
        $message = Find-ByAutomationId -Root $root -AutomationId "1001"
        $button = Find-ByAutomationId -Root $root -AutomationId "1002"
        $password = Find-ByAutomationId -Root $root -AutomationId "1003"
        $status = Find-ByAutomationId -Root $root -AutomationId "1004"

        if ($TaskId -eq "fixture-window-contract") {
            $actual = [ordered]@{
                title = $root.Current.Name
                windowAutomationId = $root.Current.AutomationId
                processId = $root.Current.ProcessId
                messageAutomationId = $message.Current.AutomationId
                buttonAutomationId = $button.Current.AutomationId
                passwordAutomationId = $password.Current.AutomationId
                statusAutomationId = $status.Current.AutomationId
            }
            return New-PassResult -Kind "UIA identity and control contract" `
                -Expected @{
                    title = "Cortex Computer Use Fixture"
                    windowAutomationId = "computer-use-fixture-window"
                    processId = $process.Id
                    controls = @("1001", "1002", "1003", "1004")
                } -Actual $actual -Details @{ pid = $process.Id }
        }

        if ($TaskId -eq "fixture-value-commit") {
            $expected = "commit-$([guid]::NewGuid().ToString('N'))"
            (Get-ValuePattern -Element $message).SetValue($expected)
            (Get-InvokePattern -Element $button).Invoke()
            $null = Wait-Until -TimeoutMilliseconds 5000 -FailureMessage (
                "Fixture status did not change to the exact input value."
            ) -Condition {
                return $status.GetCurrentPropertyValue(
                    [Windows.Automation.AutomationElement]::NameProperty
                ) -eq $expected
            }
            $actual = $status.GetCurrentPropertyValue(
                [Windows.Automation.AutomationElement]::NameProperty
            )
            return New-PassResult -Kind "UIA ValuePattern plus InvokePattern state transition" `
                -Expected $expected -Actual $actual -Details @{ pid = $process.Id }
        }

        if ($TaskId -eq "fixture-long-input") {
            $expected = "L" * 4096
            (Get-ValuePattern -Element $message).SetValue($expected)
            (Get-InvokePattern -Element $button).Invoke()
            $null = Wait-Until -TimeoutMilliseconds 5000 -FailureMessage (
                "Fixture did not copy the 4096-character input."
            ) -Condition {
                return $status.GetCurrentPropertyValue(
                    [Windows.Automation.AutomationElement]::NameProperty
                ) -eq $expected
            }
            $actual = $status.GetCurrentPropertyValue(
                [Windows.Automation.AutomationElement]::NameProperty
            )
            return New-PassResult -Kind "bounded large text round trip" `
                -Expected @{ length = 4096; sha256 = (Get-Sha256Text $expected) } `
                -Actual @{ length = $actual.Length; sha256 = (Get-Sha256Text $actual) } `
                -Details @{ pid = $process.Id }
        }

        if ($TaskId -eq "fixture-secure-field") {
            $actual = [ordered]@{
                isPassword = $password.Current.IsPassword
                name = $password.Current.Name
                itemStatus = $password.GetCurrentPropertyValue(
                    [Windows.Automation.AutomationElement]::ItemStatusProperty
                )
            }
            if (-not $actual.isPassword) {
                throw "Secure fixture field did not expose UIA IsPassword=true."
            }
            if ($actual.name -notmatch "Password" -or $actual.itemStatus -notmatch "Password") {
                throw "Secure fixture field labels do not identify password semantics."
            }
            return New-PassResult -Kind "secure field metadata; no value written" `
                -Expected @{
                    isPassword = $true
                    nameContains = "Password"
                    itemStatusContains = "Password"
                } -Actual $actual -Details @{ pid = $process.Id }
        }

        if ($TaskId -eq "fixture-readonly-status") {
            $valuePattern = Get-ValuePattern -Element $status
            $name = $status.GetCurrentPropertyValue(
                [Windows.Automation.AutomationElement]::NameProperty
            )
            $itemStatus = $status.GetCurrentPropertyValue(
                [Windows.Automation.AutomationElement]::ItemStatusProperty
            )
            $actual = [ordered]@{
                isReadOnly = $valuePattern.Current.IsReadOnly
                name = $name
                itemStatus = $itemStatus
            }
            if (-not $actual.isReadOnly) {
                throw "Verification status did not expose ValuePattern.IsReadOnly=true."
            }
            if ($name -ne $initialStatus -or $itemStatus -ne $initialStatus) {
                throw "Verification status did not expose the exact initial state."
            }
            return New-PassResult -Kind "read-only ValuePattern and exact accessible state" `
                -Expected @{
                    isReadOnly = $true
                    name = $initialStatus
                    itemStatus = $initialStatus
                } -Actual $actual -Details @{ pid = $process.Id }
        }

        throw "Unknown fixture acceptance task: $TaskId"
    }
    finally {
        $null = Stop-TestProcess -Process $process
    }
}

function Get-Sha256Text {
    param([string]$Text)

    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
        return ([System.BitConverter]::ToString($sha.ComputeHash($bytes))).Replace("-", "")
    }
    finally {
        $sha.Dispose()
    }
}

function Get-Sha256File {
    param([string]$Path)

    $sha = [System.Security.Cryptography.SHA256]::Create()
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        return ([System.BitConverter]::ToString($sha.ComputeHash($stream))).Replace("-", "")
    }
    finally {
        $stream.Dispose()
        $sha.Dispose()
    }
}

function Write-E2EStage {
    param([string]$Message)

    if ($env:CORTEX_E2E_DEBUG -eq "1") {
        Write-Host "E2E_STAGE $Message"
    }
}

function Write-E2EDebugTree {
    param(
        [Windows.Automation.AutomationElement]$Root,
        [string]$Label
    )

    if ($env:CORTEX_E2E_DEBUG -ne "1") {
        return
    }
    $elements = Get-AllDescendants -Root $Root
    $index = 0
    foreach ($element in $elements) {
        $typeName = $element.Current.ControlType.ProgrammaticName
        $name = $element.Current.Name
        $automationId = $element.Current.AutomationId
        $interesting = (
            $index -lt 250 -or
            $name -match "A1|A2|Excel acceptance seed|名称框|编辑栏|网格" -or
            $typeName -eq "ControlType.DataGrid" -or
            $typeName -eq "ControlType.Table"
        )
        if ($interesting -and ($name -or $automationId)) {
            Write-Host (
                "E2E_TREE $Label [$index] type=$typeName " +
                "name='$name' id='$automationId'"
            )
        }
        $index++
    }
}

function Start-IsolatedChrome {
    param(
        [string]$Url,
        [string]$ProfileDirectory
    )

    if ([string]::IsNullOrWhiteSpace($script:ChromeExe)) {
        throw "Google Chrome was not found."
    }
    $arguments = @(
        "--user-data-dir=$(Quote-ProcessArgument $ProfileDirectory)"
        "--app=$(Quote-ProcessArgument $Url)"
        "--no-first-run"
        "--no-default-browser-check"
        "--disable-background-networking"
        "--disable-component-update"
        "--disable-default-apps"
        "--disable-extensions"
        "--disable-sync"
        "--force-renderer-accessibility"
        "--metrics-recording-only"
        "--remote-debugging-port=0"
        "--remote-allow-origins=*"
        "--new-window"
    ) -join " "
    $launcher = Start-ExactProcess -FilePath $script:ChromeExe -Arguments $arguments
    $profileToken = "--user-data-dir=" + (Quote-ProcessArgument $ProfileDirectory)
    $browser = Wait-Until -TimeoutMilliseconds 20000 -FailureMessage (
        "Chrome did not create an owned browser process for the isolated profile."
    ) -PollMilliseconds 150 -Condition {
        $candidates = Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" |
            Where-Object {
                $_.CommandLine -and
                $_.CommandLine.IndexOf(
                    $ProfileDirectory,
                    [System.StringComparison]::OrdinalIgnoreCase
                ) -ge 0 -and
                $_.CommandLine -notmatch "(?i)(?:^|\s)--type="
            }
        foreach ($candidate in $candidates) {
            try {
                $process = Get-Process -Id $candidate.ProcessId -ErrorAction Stop
                $process.Refresh()
                $null = Assert-ExactProcessPath -Process $process -Allowlist @($script:ChromeExe)
                if (-not $process.HasExited -and
                    $process.MainWindowHandle -ne [IntPtr]::Zero) {
                    return $process
                }
            }
            catch {
            }
        }
        return $false
    }
    $launcher.Refresh()
    $commandLine = Get-ProcessCommandLine -ProcessId $browser.Id
    if ($commandLine.IndexOf(
            $ProfileDirectory,
            [System.StringComparison]::OrdinalIgnoreCase
        ) -lt 0) {
        throw "Chrome browser command line does not contain the exact isolated profile."
    }
    if ($commandLine -match "(?i)(?:^|\s)--type=") {
        throw "The isolated Chrome browser pid unexpectedly identifies as a child process."
    }
    if ($commandLine.IndexOf(
            $profileToken,
            [System.StringComparison]::OrdinalIgnoreCase
        ) -lt 0) {
        throw "Chrome browser command line lacks the exact --user-data-dir argument."
    }
    return [pscustomobject]@{
        Process = $browser
        LauncherProcessId = $launcher.Id
        CommandLine = $commandLine
        ProfileDirectory = $ProfileDirectory
    }
}

function Wait-ChromeDebugTarget {
    param(
        [System.Diagnostics.Process]$Process,
        [string]$ProfileDirectory,
        [string]$ExpectedLocalPath
    )

    $portPath = Join-Path $ProfileDirectory "DevToolsActivePort"
    $target = Wait-Until -TimeoutMilliseconds 20000 -FailureMessage (
        "Chrome did not expose an isolated loopback DevTools endpoint."
    ) -PollMilliseconds 150 -Condition {
        if (-not (Test-Path -LiteralPath $portPath -PathType Leaf)) {
            return $false
        }
        try {
            $lines = Get-Content -LiteralPath $portPath
            if ($lines.Count -lt 2) {
                return $false
            }
            $port = [int]$lines[0]
            $targets = Invoke-RestMethod -Uri "http://127.0.0.1:$port/json" `
                -TimeoutSec 2
            foreach ($candidate in @($targets)) {
                if ($candidate.type -ne "page") {
                    continue
                }
                $uri = [System.Uri]$candidate.url
                if (-not $uri.IsFile) {
                    continue
                }
                if ((Get-NormalizedPath -Path $uri.LocalPath) -eq
                    (Get-NormalizedPath -Path $ExpectedLocalPath)) {
                    $candidate | Add-Member -NotePropertyName debugPort `
                        -NotePropertyValue $port -Force
                    return $candidate
                }
            }
        }
        catch {
        }
        return $false
    }

    $Process.Refresh()
    if ($Process.HasExited) {
        throw "Chrome exited before the isolated DevTools page target became ready."
    }
    return $target
}

function Open-CdpConnection {
    param([string]$WebSocketUrl)

    $socket = [System.Net.WebSockets.ClientWebSocket]::new()
    $timeout = [System.Threading.CancellationTokenSource]::new(
        [TimeSpan]::FromSeconds(10)
    )
    try {
        $null = $socket.ConnectAsync(
            [System.Uri]$WebSocketUrl,
            $timeout.Token
        ).GetAwaiter().GetResult()
    }
    finally {
        $timeout.Dispose()
    }
    $script:CdpSocket = $socket
    $script:CdpMessageId = 0
}

function Close-CdpConnection {
    if ($null -eq $script:CdpSocket) {
        return
    }
    try {
        if ($script:CdpSocket.State -eq
            [System.Net.WebSockets.WebSocketState]::Open) {
            $timeout = [System.Threading.CancellationTokenSource]::new(
                [TimeSpan]::FromSeconds(5)
            )
            try {
                $null = $script:CdpSocket.CloseAsync(
                    [System.Net.WebSockets.WebSocketCloseStatus]::NormalClosure,
                    "acceptance task complete",
                    $timeout.Token
                ).GetAwaiter().GetResult()
            }
            finally {
                $timeout.Dispose()
            }
        }
    }
    catch {
    }
    finally {
        $script:CdpSocket.Dispose()
        $script:CdpSocket = $null
    }
}

function Invoke-CdpCommand {
    param(
        [string]$Method,
        [object]$Parameters
    )

    if ($null -eq $script:CdpSocket -or
        $script:CdpSocket.State -ne [System.Net.WebSockets.WebSocketState]::Open) {
        throw "CDP WebSocket is not connected."
    }
    $script:CdpMessageId++
    $id = $script:CdpMessageId
    $payload = [ordered]@{
        id = $id
        method = $Method
        params = $Parameters
    }
    if ($null -eq $Parameters) {
        $payload.params = @{}
    }
    $payloadJson = $payload | ConvertTo-Json -Compress -Depth 12
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($payloadJson)
    $null = $script:CdpSocket.SendAsync(
        [System.ArraySegment[byte]]::new($bytes),
        [System.Net.WebSockets.WebSocketMessageType]::Text,
        $true,
        [System.Threading.CancellationToken]::None
    ).GetAwaiter().GetResult()

    while ($true) {
        $buffer = New-Object byte[] 65536
        $builder = [System.Text.StringBuilder]::new()
        do {
            $timeout = [System.Threading.CancellationTokenSource]::new(
                [TimeSpan]::FromSeconds(10)
            )
            try {
            $result = $script:CdpSocket.ReceiveAsync(
                [System.ArraySegment[byte]]::new($buffer),
                $timeout.Token
            ).GetAwaiter().GetResult()
            if ($result.Count -eq 0) {
                continue
            }
            }
            finally {
                $timeout.Dispose()
            }
            if ($result.MessageType -eq
                [System.Net.WebSockets.WebSocketMessageType]::Close) {
                throw "CDP WebSocket closed before the command completed."
            }
            $text = [System.Text.Encoding]::UTF8.GetString(
                $buffer,
                0,
                $result.Count
            )
            $null = $builder.Append($text)
        } while (-not $result.EndOfMessage)

        $message = $builder.ToString() | ConvertFrom-Json
        if ($null -eq $message.PSObject.Properties["id"] -or
            $message.id -ne $id) {
            continue
        }
        if ($null -ne $message.PSObject.Properties["error"]) {
            throw "CDP $Method failed: $($message.error.message)"
        }
        return $message.result
    }
}

function Invoke-CdpEvaluate {
    param([string]$Expression)

    $result = Invoke-CdpCommand -Method "Runtime.evaluate" -Parameters ([ordered]@{
        expression = $Expression
        returnByValue = $true
        awaitPromise = $true
    })
    Write-E2EStage ("cdp-result:" + ($result | ConvertTo-Json -Compress -Depth 8))
    if ($null -ne $result.PSObject.Properties["exceptionDetails"]) {
        throw "CDP expression failed: $($result.exceptionDetails.text)"
    }
    if ($null -eq $result.PSObject.Properties["result"]) {
        return $null
    }
    $value = $result.result
    if ($null -ne $value.PSObject.Properties["value"]) {
        return $value.value
    }
    return $null
}

function Convert-CdpStructuredValue {
    param([object]$Value)

    if ($null -eq $Value) {
        return $null
    }
    if ($Value -is [string]) {
        $trimmed = $Value.Trim()
        if ($trimmed.StartsWith("{") -or $trimmed.StartsWith("[")) {
            return ($trimmed | ConvertFrom-Json)
        }
    }
    return $Value
}

function Invoke-ChromeAction {
    param(
        [string]$TaskId,
        [string[]]$Allowlist
    )

    if ([string]::IsNullOrWhiteSpace($script:ChromeExe)) {
        return New-SkipResult -Reason "Google Chrome is not installed."
    }
    if (-not (Test-Path -LiteralPath $script:ChromeHtml -PathType Leaf)) {
        throw "Chrome acceptance fixture is missing: $script:ChromeHtml"
    }

    $baseline = Get-AppWindowSnapshot -ProcessNames @("chrome")
    $profileDirectory = New-TestDirectory -Purpose "chrome-profile"
    $htmlUri = [System.Uri]::new($script:ChromeHtml).AbsoluteUri
    $process = $null
    $chromeLaunch = $null
    $target = $null
    $script:CdpSocket = $null
    try {
        Write-E2EStage "chrome:start"
        $chromeLaunch = Start-IsolatedChrome `
            -Url $htmlUri -ProfileDirectory $profileDirectory
        $process = $chromeLaunch.Process
        $null = Assert-ExactProcessPath -Process $process -Allowlist $Allowlist
        Write-E2EStage "chrome:process:$($process.Id)"
        $handle = Wait-MainWindow -Process $process -TimeoutMilliseconds 20000 `
            -TitlePattern "Cortex Chrome Acceptance"
        $windowEvidence = Assert-ExactTestWindow `
            -Process $process `
            -WindowHandle $handle `
            -BaselineSnapshot $baseline
        $windowEvidence.launcherProcessId = $chromeLaunch.LauncherProcessId
        $windowEvidence.commandLine = $chromeLaunch.CommandLine
        $windowEvidence.profileDirectory = $chromeLaunch.ProfileDirectory
        Write-E2EStage "chrome:window"
        $target = Wait-ChromeDebugTarget -Process $process `
            -ProfileDirectory $profileDirectory -ExpectedLocalPath $script:ChromeHtml
        Write-E2EStage "chrome:target:$($target.debugPort)"
        Open-CdpConnection -WebSocketUrl $target.webSocketDebuggerUrl
        Write-E2EStage "chrome:cdp-connected"
        $null = Invoke-CdpCommand -Method "Runtime.enable" -Parameters $null
        Write-E2EStage "chrome:runtime-enabled"
        $null = Invoke-CdpEvaluate -Expression "document.readyState"
        Write-E2EStage "chrome:document-ready"

        if ($TaskId -eq "chrome-launch-observe") {
            $actualJson = Invoke-CdpEvaluate -Expression @"
(() => ({
  title: document.title,
  readyState: document.readyState,
  messageInput: Boolean(document.querySelector('[aria-label="Message input"]')),
  commitButton: Boolean(document.querySelector('[aria-label="Commit message"]')),
  initialStatus: document.querySelector('#verificationStatus')?.textContent ?? null
}))()
"@
            Write-E2EStage "chrome:observe-result:$actualJson"
            if ([string]::IsNullOrWhiteSpace($actualJson)) {
                throw "Chrome page discovery verifier returned no value."
            }
            $actual = Convert-CdpStructuredValue -Value $actualJson
            if (-not $actual.messageInput -or -not $actual.commitButton -or
                $actual.initialStatus -ne "Ready") {
                throw "Chrome acceptance page did not satisfy the browser DOM contract."
            }
            return New-PassResult -Kind "isolated Chrome page discovery through loopback CDP" `
                -Expected @{
                    title = "Cortex Chrome Acceptance"
                    readyState = "complete"
                    messageInput = $true
                    commitButton = $true
                    initialStatus = "Ready"
                } -Actual $actual -Details @{
                    processId = $process.Id
                    engine = "PowerShell + Chrome DevTools Protocol"
                    endpoint = "127.0.0.1:$($target.debugPort)"
                    isolation = $windowEvidence
                    rawUiaLimit = (
                        "Chrome exposed only native browser chrome to raw UIA " +
                        "on this host; the process-scoped loopback CDP page target " +
                        "was used for browser acceptance."
                    )
                }
        }

        if ($TaskId -eq "chrome-input-commit") {
            $expected = "chrome-$([guid]::NewGuid().ToString('N'))"
            $actualJson = Invoke-CdpEvaluate -Expression @"
(() => {
  const message = document.querySelector('[aria-label="Message input"]');
  message.value = '$expected';
  document.querySelector('[aria-label="Commit message"]').click();
  return document.querySelector('#verificationStatus').textContent;
})()
"@
            $actual = [string]$actualJson
            Write-E2EStage "chrome:input-result:$actualJson"
            if ($actual -ne $expected) {
                throw "Chrome verification status '$actual' did not equal '$expected'."
            }
            return New-PassResult -Kind "isolated Chrome form input and button state transition" `
                -Expected $expected -Actual $actual `
                -Details @{
                    processId = $process.Id
                    engine = "loopback CDP"
                    isolation = $windowEvidence
                }
        }

        if ($TaskId -eq "chrome-checkbox-toggle") {
            $actualJson = Invoke-CdpEvaluate -Expression @"
(() => {
  document.querySelector('[aria-label="Accept terms"]').click();
  document.querySelector('[aria-label="Record checkbox state"]').click();
  return document.querySelector('#checkboxStatus').textContent;
})()
"@
            $actual = [string]$actualJson
            if ($actual -ne "checked") {
                throw "Chrome checkbox state did not reach 'checked'."
            }
            return New-PassResult -Kind "isolated Chrome checkbox toggle and state verification" `
                -Expected "checked" -Actual $actual `
                -Details @{
                    processId = $process.Id
                    engine = "loopback CDP"
                    isolation = $windowEvidence
                }
        }

        if ($TaskId -eq "chrome-secure-field") {
            $actualJson = Invoke-CdpEvaluate -Expression @"
(() => {
  const field = document.querySelector('[aria-label="Password input"]');
  return {
    type: field?.type ?? null,
    value: field?.value ?? null,
    accessibleName: field?.getAttribute('aria-label') ?? null
  };
})()
"@
            if ([string]::IsNullOrWhiteSpace($actualJson)) {
                throw "Chrome secure-field verifier returned no value."
            }
            $actual = Convert-CdpStructuredValue -Value $actualJson
            if ($actual.type -ne "password" -or $actual.value -ne "" -or
                $actual.accessibleName -ne "Password input") {
                throw "Chrome secure field semantics or empty-value invariant failed."
            }
            return New-PassResult -Kind "isolated Chrome secure field semantics; no value written" `
                -Expected @{
                    type = "password"
                    value = ""
                    accessibleName = "Password input"
                } -Actual $actual -Details @{
                    processId = $process.Id
                    engine = "loopback CDP"
                    isolation = $windowEvidence
                }
        }

        if ($TaskId -eq "chrome-link-navigation") {
            $null = Invoke-CdpEvaluate -Expression (
                'document.querySelector(''[aria-label="Open second page"]'').click(); true'
            )
            $heading = Wait-Until -TimeoutMilliseconds 12000 -FailureMessage (
                "Chrome did not reach the second local page."
            ) -PollMilliseconds 150 -Condition {
                $value = Invoke-CdpEvaluate -Expression (
                    "document.querySelector('h1')?.textContent ?? ''"
                )
                if ($value -eq "Second page reached") {
                    return [pscustomobject]@{
                        heading = $value
                        title = Invoke-CdpEvaluate -Expression "document.title"
                        url = Invoke-CdpEvaluate -Expression "location.href"
                    }
                }
                return $false
            }
            $targetUri = [System.Uri]$heading.url
            if (-not $targetUri.IsFile -or
                [System.IO.Path]::GetFileName($targetUri.LocalPath) -ne "page2.html") {
                throw "Chrome navigation left the local acceptance fixture."
            }
            return New-PassResult -Kind "isolated Chrome local navigation" `
                -Expected @{
                    heading = "Second page reached"
                    localFile = "page2.html"
                } -Actual $heading -Details @{
                    processId = $process.Id
                    engine = "loopback CDP"
                    isolation = $windowEvidence
                }
        }

        throw "Unknown Chrome acceptance task: $TaskId"
    }
    finally {
        Close-CdpConnection
        if ($null -ne $process) {
            $null = Stop-TestProcess -Process $process
        }
        Remove-TestDirectory -Path $profileDirectory
    }
}

function Get-EditorElement {
    param([Windows.Automation.AutomationElement]$Root)

    $candidates = @()
    $all = Get-AllDescendants -Root $Root
    foreach ($element in $all) {
        $type = $element.Current.ControlType
        if ($type -eq [Windows.Automation.ControlType]::Document -or
            $type -eq [Windows.Automation.ControlType]::Edit) {
            if (-not $element.Current.IsOffscreen) {
                $bounds = $element.Current.BoundingRectangle
                $candidates += [pscustomobject]@{
                    Element = $element
                    Area = [double]$bounds.Width * [double]$bounds.Height
                }
            }
        }
    }
    $candidate = $candidates | Sort-Object Area -Descending | Select-Object -First 1
    if ($null -eq $candidate) {
        throw "No editable UIA Document or Edit control was found."
    }
    return $candidate.Element
}

function Get-UiaRuntimeIdKey {
    param([Windows.Automation.AutomationElement]$Element)

    return (@($Element.GetRuntimeId()) -join ".")
}

function Get-NotepadTabSnapshot {
    param([Windows.Automation.AutomationElement]$Root)

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::ControlTypeProperty,
        [Windows.Automation.ControlType]::TabItem
    )
    $tabs = $Root.FindAll(
        [Windows.Automation.TreeScope]::Descendants,
        $condition
    )
    $snapshot = for ($index = 0; $index -lt $tabs.Count; $index++) {
        $tab = $tabs.Item($index)
        $selection = $null
        $selectionPattern = $null
        if (-not $tab.TryGetCurrentPattern(
                [Windows.Automation.SelectionItemPattern]::Pattern,
                [ref]$selectionPattern)) {
            throw "Notepad TabItem '$($tab.Current.Name)' has no SelectionItemPattern."
        }
        $selection = [Windows.Automation.SelectionItemPattern]$selectionPattern
        $labels = @(
            $tab.FindAll(
                [Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::ControlTypeProperty,
                    [Windows.Automation.ControlType]::Text
                )
            ) | ForEach-Object { $_ }
        )
        [ordered]@{
            index = $index
            runtimeId = @($tab.GetRuntimeId())
            runtimeIdKey = Get-UiaRuntimeIdKey -Element $tab
            name = [string]$tab.Current.Name
            automationId = [string]$tab.Current.AutomationId
            className = [string]$tab.Current.ClassName
            selected = [bool]$selection.Current.IsSelected
            enabled = [bool]$tab.Current.IsEnabled
            offscreen = [bool]$tab.Current.IsOffscreen
            itemStatus = [string]$tab.Current.ItemStatus
            labelCount = $labels.Count
            labelRuntimeIds = @(
                $labels | ForEach-Object {
                    Get-UiaRuntimeIdKey -Element $_
                }
            )
            labelNames = @($labels | ForEach-Object { [string]$_.Current.Name })
        }
    }
    return @($snapshot)
}

function Find-UniqueUiaElement {
    param(
        [Windows.Automation.AutomationElement]$Scope,
        [Windows.Automation.ControlType]$ControlType,
        [string]$AutomationId
    )

    $conditions = [Windows.Automation.Condition[]]@(
        [Windows.Automation.PropertyCondition]::new(
            [Windows.Automation.AutomationElement]::ControlTypeProperty,
            $ControlType
        ),
        [Windows.Automation.PropertyCondition]::new(
            [Windows.Automation.AutomationElement]::AutomationIdProperty,
            $AutomationId
        )
    )
    $matches = @(
        $Scope.FindAll(
            [Windows.Automation.TreeScope]::Descendants,
            [Windows.Automation.AndCondition]::new($conditions)
        ) | ForEach-Object { $_ }
    )
    if ($matches.Count -ne 1) {
        throw (
            "Expected exactly one UIA $($ControlType.ProgrammaticName) with " +
            "AutomationId '$AutomationId'; found $($matches.Count)."
        )
    }
    return $matches[0]
}

function Get-UiaElementByRuntimeId {
    param(
        [Windows.Automation.AutomationElement]$Root,
        [int[]]$RuntimeId
    )

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::RuntimeIdProperty,
        $RuntimeId
    )
    $matches = @(
        $Root.FindAll(
            [Windows.Automation.TreeScope]::Descendants,
            $condition
        ) | ForEach-Object { $_ }
    )
    if ($matches.Count -ne 1) {
        throw (
            "Expected exactly one UIA element for RuntimeId '$($RuntimeId -join ".")'; " +
            "found $($matches.Count)."
        )
    }
    return $matches[0]
}

function Get-NotepadInvokePattern {
    param(
        [Windows.Automation.AutomationElement]$Element,
        [string]$Description
    )

    $pattern = $null
    if (-not $Element.TryGetCurrentPattern(
            [Windows.Automation.InvokePattern]::Pattern,
            [ref]$pattern)) {
        throw "$Description does not expose InvokePattern."
    }
    return [Windows.Automation.InvokePattern]$pattern
}

function Get-NotepadVisibleDocuments {
    param([Windows.Automation.AutomationElement]$Root)

    try {
        $condition = [Windows.Automation.PropertyCondition]::new(
            [Windows.Automation.AutomationElement]::ControlTypeProperty,
            [Windows.Automation.ControlType]::Document
        )
        return @(
            $Root.FindAll(
                [Windows.Automation.TreeScope]::Descendants,
                $condition
            ) | Where-Object { -not $_.Current.IsOffscreen } | ForEach-Object { $_ }
        )
    }
    catch {
        # During native dialog transitions UIA can briefly fail with an
        # opaque "Unrecognized error"; treat that as no stable observation yet.
        return @()
    }
}

function Test-NotepadTabSnapshotsEqual {
    param(
        [object[]]$Expected,
        [object[]]$Actual
    )

    if ($Expected.Count -ne $Actual.Count) {
        return $false
    }
    for ($index = 0; $index -lt $Expected.Count; $index++) {
        if ([string]$Expected[$index].runtimeIdKey -ne
                [string]$Actual[$index].runtimeIdKey -or
            [string]$Expected[$index].name -ne [string]$Actual[$index].name -or
            [bool]$Expected[$index].selected -ne [bool]$Actual[$index].selected -or
            [string]$Expected[$index].itemStatus -ne
                [string]$Actual[$index].itemStatus -or
            (@($Expected[$index].labelRuntimeIds) -join "|") -ne
                (@($Actual[$index].labelRuntimeIds) -join "|") -or
            (@($Expected[$index].labelNames) -join "|") -ne
                (@($Actual[$index].labelNames) -join "|")) {
            return $false
        }
    }
    return $true
}

function New-NotepadTabBaseline {
    param(
        [object]$AppSnapshot,
        [string[]]$Allowlist
    )

    $processIds = @($AppSnapshot.processIds | ForEach-Object { [int]$_ })
    $windows = @($AppSnapshot.windows)
    # Packaged Notepad can keep a windowless launcher process beside the real
    # UI process. Process count alone therefore cannot prove UI isolation; the
    # stable boundary is exactly one visible top-level window owned by one
    # allowlisted process.
    if ($windows.Count -ne 1) {
        return [ordered]@{
            ok = $false
            reason = (
                "Expected exactly one visible top-level Notepad window before " +
                "creating a test tab."
            )
            details = [ordered]@{
                processIds = $processIds
                windows = $windows
            }
        }
    }

    $window = $windows[0]
    $process = Get-Process -Id ([int]$window.processId) -ErrorAction Stop
    $null = Assert-ExactProcessPath -Process $process -Allowlist $Allowlist
    if ([int]$window.processId -ne $process.Id) {
        throw (
            "The visible Notepad window belongs to pid $($window.processId), " +
            "which could not be resolved to the expected process."
        )
    }
    $windowHandle = Convert-WindowHandle -WindowHandle ([string]$window.handle)
    $root = Get-WindowRootAtHandle `
        -Process $process `
        -WindowHandle $windowHandle `
        -Allowlist $Allowlist

    $tabs = @(Get-NotepadTabSnapshot -Root $root)
    if ($tabs.Count -eq 0) {
        return [ordered]@{
            ok = $false
            reason = "Notepad did not expose any TabItem controls."
            details = [ordered]@{ window = $window }
        }
    }
    $selectedTabs = @($tabs | Where-Object { [bool]$_.selected })
    if ($selectedTabs.Count -ne 1) {
        return [ordered]@{
            ok = $false
            reason = (
                "Expected exactly one selected Notepad TabItem before adding a " +
                "test tab; found $($selectedTabs.Count)."
            )
            details = [ordered]@{ tabs = $tabs }
        }
    }

    try {
        $addButton = Find-UniqueUiaElement `
            -Scope $root `
            -ControlType ([Windows.Automation.ControlType]::Button) `
            -AutomationId "AddButton"
        $addInvoke = Get-NotepadInvokePattern `
            -Element $addButton `
            -Description "Notepad AddButton"
        $parent = [Windows.Automation.TreeWalker]::ControlViewWalker.GetParent(
            $addButton
        )
        if ($null -eq $parent -or
            $parent.Current.ControlType -ne [Windows.Automation.ControlType]::Tab -or
            $parent.Current.AutomationId -ne "Tabs") {
            throw "Notepad AddButton is not a direct child of the exact Tabs control."
        }

        $activeTab = Get-UiaElementByRuntimeId `
            -Root $root `
            -RuntimeId ([int[]]$selectedTabs[0].runtimeId)
        $documents = @(Get-NotepadVisibleDocuments -Root $root)
        if ($documents.Count -ne 1) {
            throw (
                "Expected exactly one visible Notepad Document for the selected " +
                "baseline tab; found $($documents.Count)."
            )
        }
    }
    catch {
        return [ordered]@{
            ok = $false
            reason = (
                "Notepad add/close/document anchors were not uniquely provable: " +
                $_.Exception.Message
            )
            details = [ordered]@{
                processId = $process.Id
                windowId = Get-WindowHandleHex -WindowHandle $windowHandle
                tabs = $tabs
                selectedTabCount = $selectedTabs.Count
            }
        }
    }

    return [ordered]@{
        ok = $true
        Process = $process
        Root = $root
        WindowHandle = $windowHandle
        Window = $window
        AppSnapshot = $AppSnapshot
        Tabs = $tabs
        ActiveTab = $activeTab
        AddButton = $addButton
        AddInvoke = $addInvoke
        ActiveDocument = $documents[0]
        Evidence = [ordered]@{
            processId = $process.Id
            windowId = Get-WindowHandleHex -WindowHandle $windowHandle
            windowTitle = [string]$window.title
            tabsBefore = $tabs
            activeTabRuntimeId = [string]$selectedTabs[0].runtimeIdKey
            addButton = [ordered]@{
                automationId = [string]$addButton.Current.AutomationId
                className = [string]$addButton.Current.ClassName
                runtimeId = Get-UiaRuntimeIdKey -Element $addButton
            }
            baselineDocumentRuntimeId = Get-UiaRuntimeIdKey -Element $documents[0]
        }
    }
}

function New-NotepadTestTab {
    param([object]$Baseline)

    $before = @(Get-NotepadTabSnapshot -Root $Baseline.Root)
    if (-not (Test-NotepadTabSnapshotsEqual `
            -Expected $Baseline.Tabs `
            -Actual $before)) {
        return [ordered]@{
            ok = $false
            reason = (
                "Notepad tab state changed between preflight and AddButton " +
                "invocation; no AddButton input was sent."
            )
            details = [ordered]@{
                baselineTabs = $Baseline.Tabs
                currentTabs = $before
            }
        }
    }

    $addButton = Get-UiaElementByRuntimeId `
        -Root $Baseline.Root `
        -RuntimeId ([int[]]$Baseline.AddButton.GetRuntimeId())
    if ($addButton.Current.ControlType -ne [Windows.Automation.ControlType]::Button -or
        $addButton.Current.AutomationId -ne "AddButton") {
        return [ordered]@{
            ok = $false
            reason = "The exact preflight AddButton identity changed before invocation."
            details = [ordered]@{
                currentAutomationId = [string]$addButton.Current.AutomationId
                currentRuntimeId = Get-UiaRuntimeIdKey -Element $addButton
            }
        }
    }

    $addInvoke = Get-NotepadInvokePattern `
        -Element $addButton `
        -Description "Exact Notepad AddButton"
    $addInvoke.Invoke()

    $deadline = [DateTime]::UtcNow.AddSeconds(8)
    do {
        Start-Sleep -Milliseconds 100
        $after = @(Get-NotepadTabSnapshot -Root $Baseline.Root)
        $newTabs = @(
            $after | Where-Object {
                $before.runtimeIdKey -notcontains $_.runtimeIdKey
            }
        )
        $selected = @($after | Where-Object { [bool]$_.selected })
    } while ((
            $after.Count -ne $before.Count + 1 -or
            $newTabs.Count -ne 1 -or
            $selected.Count -ne 1 -or
            [string]$selected[0].runtimeIdKey -ne [string]$newTabs[0].runtimeIdKey
        ) -and [DateTime]::UtcNow -lt $deadline)

    if ($after.Count -ne $before.Count + 1 -or
        $newTabs.Count -ne 1 -or
        $selected.Count -ne 1 -or
        [string]$selected[0].runtimeIdKey -ne [string]$newTabs[0].runtimeIdKey) {
        throw (
            "Exact new-tab proof failed after AddButton: before=$($before.Count), " +
            "after=$($after.Count), new=$($newTabs.Count), selected=$($selected.Count)."
        )
    }

    $newTab = Get-UiaElementByRuntimeId `
        -Root $Baseline.Root `
        -RuntimeId ([int[]]$newTabs[0].runtimeId)
    $deadline = [DateTime]::UtcNow.AddSeconds(8)
    do {
        Start-Sleep -Milliseconds 100
        $documents = @(Get-NotepadVisibleDocuments -Root $Baseline.Root)
    } while ((
            $documents.Count -ne 1 -or
            (Get-UiaRuntimeIdKey -Element $documents[0]) -eq
                [string]$Baseline.Evidence.baselineDocumentRuntimeId
        ) -and [DateTime]::UtcNow -lt $deadline)
    if ($documents.Count -ne 1 -or
        (Get-UiaRuntimeIdKey -Element $documents[0]) -eq
            [string]$Baseline.Evidence.baselineDocumentRuntimeId) {
        throw (
            "The active Document did not transition exactly once with the new tab: " +
            "count=$($documents.Count)."
        )
    }

    return [ordered]@{
        ok = $true
        Tab = $newTab
        TabRuntimeId = [int[]]$newTabs[0].runtimeId
        TabRuntimeIdKey = [string]$newTabs[0].runtimeIdKey
        TabNameAfterAdd = [string]$newTabs[0].name
        Document = $documents[0]
        BlankDocumentRuntimeId = Get-UiaRuntimeIdKey -Element $documents[0]
        TabsAfterAdd = $after
        Evidence = [ordered]@{
            tab = [ordered]@{
                runtimeId = [string]$newTabs[0].runtimeIdKey
                nameAfterAdd = [string]$newTabs[0].name
                selected = $true
                uniqueNewTab = $true
                uniqueActive = $true
            }
            invokeCloseSupported = $true
            blankDocumentRuntimeId = Get-UiaRuntimeIdKey -Element $documents[0]
        }
    }
}

function Get-NeededForegroundWindow {
    param(
        [IntPtr]$OriginalWindow,
        [int]$ProcessId,
        [int]$TimeoutMilliseconds = 8000
    )

    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    do {
        $foreground = [CortexComputerUseE2E.NativeMethods]::GetForegroundWindow()
        if ($foreground -ne [IntPtr]::Zero -and
            $foreground -ne $OriginalWindow -and
            [CortexComputerUseE2E.NativeMethods]::IsWindow($foreground)) {
            $ownerProcessId = [uint32]0
            $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
                $foreground,
                [ref]$ownerProcessId
            )
            if ([int]$ownerProcessId -eq $ProcessId) {
                return $foreground
            }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw (
        "The expected Notepad-owned foreground dialog did not appear within " +
        "$TimeoutMilliseconds ms."
    )
}

function Get-NotepadFileDialogAnchors {
    param(
        [IntPtr]$DialogHandle,
        [int]$ProcessId
    )

    if (-not [CortexComputerUseE2E.NativeMethods]::IsWindow($DialogHandle)) {
        throw "Notepad file dialog handle is invalid."
    }
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $attempts = 0
    $lastIssue = "the file-dialog controls had not settled"
    $dialog = $null
    $records = @()
    $fileNameRecord = $null
    $fileNameSource = $null
    $fileNameHostRecord = $null
    $openRecord = $null
    $cancelRecord = $null
    $fileNameEditCount = 0
    $saveFileNameEditCount = 0
    $fileNameFallbackCount = 0
    $openButtonCount = 0
    $cancelButtonCount = 0

    do {
        $attempts++
        if (-not [CortexComputerUseE2E.NativeMethods]::IsWindow($DialogHandle)) {
            throw "The exact Notepad file dialog disappeared while its controls settled."
        }
        $dialog = [Windows.Automation.AutomationElement]::FromHandle($DialogHandle)
        if ($null -eq $dialog -or $dialog.Current.ProcessId -ne $ProcessId) {
            throw "The foreground file dialog does not belong to the exact Notepad pid."
        }

        $records = @(
            foreach ($childHandle in @(
                    [CortexComputerUseE2E.NativeMethods]::GetChildWindows($DialogHandle)
                )) {
                [ordered]@{
                    handle = $childHandle
                    handleText = Get-WindowHandleHex -WindowHandle $childHandle
                    parentHandle = (
                        [CortexComputerUseE2E.NativeMethods]::GetParent($childHandle)
                    )
                    controlId = [CortexComputerUseE2E.NativeMethods]::GetDlgCtrlID(
                        $childHandle
                    )
                    className = (
                        [CortexComputerUseE2E.NativeMethods]::GetWindowClassName(
                            $childHandle
                        )
                    )
                    text = (
                        [CortexComputerUseE2E.NativeMethods]::GetTopLevelWindowTitle(
                            $childHandle
                        )
                    )
                    enabled = [CortexComputerUseE2E.NativeMethods]::IsWindowEnabled(
                        $childHandle
                    )
                    visible = [CortexComputerUseE2E.NativeMethods]::IsWindowVisible(
                        $childHandle
                    )
                }
            }
        )
        $fileNameEdits = @(
            $records | Where-Object {
                [int]$_.controlId -eq 1148 -and
                [string]$_.className -eq "Edit"
            }
        )
        $fileNameFallbacks = @(
            $records | Where-Object {
                [int]$_.controlId -eq 1148 -and
                ([string]$_.className -eq "ComboBox" -or
                    [string]$_.className -eq "ComboBoxEx32")
            }
        )
        $saveFileNameHosts = @(
            $records |
                Where-Object { [string]$_.className -eq "ComboBox" } |
                ForEach-Object {
                    $hostRecord = $_
                    $hostElement = $null
                    try {
                        $hostElement = (
                            [Windows.Automation.AutomationElement]::FromHandle(
                                [IntPtr]$hostRecord.handle
                            )
                        )
                    }
                    catch {
                        $hostElement = $null
                    }
                    if ($null -ne $hostElement -and
                        $hostElement.Current.ProcessId -eq $ProcessId -and
                        $hostElement.Current.AutomationId -eq "FileNameControlHost") {
                        [ordered]@{
                            host = $hostRecord
                            edits = @(
                                $records | Where-Object {
                                    [string]$_.className -eq "Edit" -and
                                    [IntPtr]$_.parentHandle -eq [IntPtr]$hostRecord.handle
                                }
                            )
                        }
                    }
                }
        )
        $openButtons = @(
            $records | Where-Object {
                [int]$_.controlId -eq 1 -and [string]$_.className -eq "Button"
            }
        )
        $cancelButtons = @(
            $records | Where-Object {
                [int]$_.controlId -eq 2 -and [string]$_.className -eq "Button"
            }
        )
        $fileNameEditCount = $fileNameEdits.Count
        $saveFileNameEditCount = if ($saveFileNameHosts.Count -eq 1) {
            @($saveFileNameHosts[0].edits).Count
        }
        else {
            $saveFileNameHosts.Count
        }
        $fileNameFallbackCount = $fileNameFallbacks.Count
        $openButtonCount = $openButtons.Count
        $cancelButtonCount = $cancelButtons.Count

        $fileNameRecord = $null
        $fileNameSource = $null
        $fileNameHostRecord = $null
        if ($fileNameEdits.Count -eq 1) {
            $fileNameRecord = $fileNameEdits[0]
            $fileNameSource = "Edit/1148"
        }
        elseif ($saveFileNameHosts.Count -eq 1 -and
            @($saveFileNameHosts[0].edits).Count -eq 1) {
            $fileNameRecord = @($saveFileNameHosts[0].edits)[0]
            $fileNameHostRecord = $saveFileNameHosts[0].host
            $fileNameSource = "SaveAs/FileNameControlHost/Edit"
        }
        elseif ($fileNameEdits.Count -eq 0 -and $fileNameFallbacks.Count -eq 1) {
            $fileNameRecord = $fileNameFallbacks[0]
            $fileNameSource = "ComboBox/1148"
        }
        elseif ($fileNameEdits.Count -gt 1) {
            $lastIssue = (
                "found $($fileNameEdits.Count) editable Edit/1148 controls " +
                "instead of one exact file-name control"
            )
        }
        elseif ($saveFileNameHosts.Count -gt 1) {
            $lastIssue = (
                "found $($saveFileNameHosts.Count) FileNameControlHost controls " +
                "instead of one exact file-name host"
            )
        }
        elseif ($saveFileNameHosts.Count -eq 1 -and
            @($saveFileNameHosts[0].edits).Count -ne 1) {
            $lastIssue = (
                "the unique FileNameControlHost contained " +
                "$(@($saveFileNameHosts[0].edits).Count) Edit children instead of one"
            )
        }
        elseif ($fileNameFallbacks.Count -gt 1) {
            $lastIssue = (
                "found $($fileNameFallbacks.Count) ComboBox/1148 fallbacks " +
                "instead of one exact file-name control"
            )
        }
        else {
            $lastIssue = (
                "found no exact Edit/1148 file-name control and no unique " +
                "ComboBox/1148 fallback"
            )
        }

        $openRecord = if ($openButtons.Count -eq 1) { $openButtons[0] } else { $null }
        $cancelRecord = if ($cancelButtons.Count -eq 1) { $cancelButtons[0] } else { $null }
        if ($null -ne $fileNameRecord -and
            $null -ne $openRecord -and
            $null -ne $cancelRecord -and
            [bool]$fileNameRecord.enabled -and
            [bool]$openRecord.enabled -and
            [bool]$cancelRecord.enabled -and
            [CortexComputerUseE2E.NativeMethods]::IsWindow(
                [IntPtr]$fileNameRecord.handle
            ) -and
            [CortexComputerUseE2E.NativeMethods]::IsWindow(
                [IntPtr]$openRecord.handle
            ) -and
            [CortexComputerUseE2E.NativeMethods]::IsWindow(
                [IntPtr]$cancelRecord.handle
            )) {
            break
        }
        if ($openButtonCount -ne 1 -or $cancelButtonCount -ne 1) {
            $lastIssue = (
                "expected one Button/1 and one Button/2 control; found " +
                "$openButtonCount and $cancelButtonCount"
            )
        }
        elseif ($null -ne $fileNameRecord) {
            $lastIssue = "one of the exact file-dialog controls was not yet enabled"
        }
        Start-Sleep -Milliseconds 100
    } while ($stopwatch.ElapsedMilliseconds -lt 5000)

    if ($null -eq $fileNameRecord -or
        $null -eq $openRecord -or
        $null -eq $cancelRecord) {
        $observedControls = (
            $records |
                ForEach-Object {
                    "id=$($_.controlId),class=$($_.className),hwnd=$($_.handleText)"
                } |
                Sort-Object
        ) -join "; "
        $uiaCandidates = @(
            $dialog.FindAll(
                [Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.Condition]::TrueCondition
            ) |
                ForEach-Object { $_ } |
                Where-Object {
                    $_.Current.ControlType -eq [Windows.Automation.ControlType]::Edit -or
                    $_.Current.ControlType -eq [Windows.Automation.ControlType]::ComboBox -or
                    $_.Current.ControlType -eq [Windows.Automation.ControlType]::Button
                } |
                ForEach-Object {
                    "type=$($_.Current.ControlType.ProgrammaticName)," +
                    "name=$($_.Current.Name),automationId=$($_.Current.AutomationId)," +
                    "hwnd=$(Get-WindowHandleHex -WindowHandle (
                        [IntPtr]$_.Current.NativeWindowHandle
                    ))"
                }
        )
        $uiaEvidence = if ($uiaCandidates.Count -gt 0) {
            $uiaCandidates -join "; "
        }
        else {
            "none"
        }
        $nativeInputEvidence = @(
            $records |
                Where-Object {
                    [string]$_.className -eq "Edit" -or
                    [string]$_.className -eq "ComboBox" -or
                    [string]$_.className -eq "ComboBoxEx32"
                } |
                ForEach-Object {
                    $element = $null
                    try {
                        $element = [Windows.Automation.AutomationElement]::FromHandle(
                            [IntPtr]$_.handle
                        )
                    }
                    catch {
                        $element = $null
                    }
                    if ($null -ne $element) {
                        "id=$($_.controlId),class=$($_.className)," +
                        "hwnd=$($_.handleText)," +
                        "type=$($element.Current.ControlType.ProgrammaticName)," +
                        "name=$($element.Current.Name)," +
                        "automationId=$($element.Current.AutomationId)"
                    }
                    else {
                        "id=$($_.controlId),class=$($_.className)," +
                        "hwnd=$($_.handleText),uia=unavailable"
                    }
                }
        )
        $nativeInputText = if ($nativeInputEvidence.Count -gt 0) {
            $nativeInputEvidence -join "; "
        }
        else {
            "none"
        }
        throw (
            "The exact Save As controls were not uniquely provable within " +
            "5000 ms after $attempts attempts: $lastIssue; Edit/1148=" +
            "$fileNameEditCount, FileNameControlHost/Edit=$saveFileNameEditCount, " +
            "ComboBox/1148=$fileNameFallbackCount, " +
            "Button/1=$openButtonCount, Button/2=$cancelButtonCount; " +
            "observed native descendants: $observedControls; " +
            "observed UIA edit/combo/button candidates: $uiaEvidence; " +
            "native input identities: $nativeInputText"
        )
    }

    $fileNameElement = $null
    $openElement = $null
    $cancelElement = $null
    try {
        $fileNameElement = [Windows.Automation.AutomationElement]::FromHandle(
            [IntPtr]$fileNameRecord.handle
        )
        $openElement = [Windows.Automation.AutomationElement]::FromHandle(
            [IntPtr]$openRecord.handle
        )
        $cancelElement = [Windows.Automation.AutomationElement]::FromHandle(
            [IntPtr]$cancelRecord.handle
        )
    }
    catch {
        $fileNameElement = $null
        $openElement = $null
        $cancelElement = $null
    }

    $valuePattern = $null
    if ($null -ne $fileNameElement -and
        $fileNameElement.Current.ProcessId -eq $ProcessId -and
        $fileNameElement.TryGetCurrentPattern(
            [Windows.Automation.ValuePattern]::Pattern,
            [ref]$valuePattern)) {
        $valuePattern = [Windows.Automation.ValuePattern]$valuePattern
        if ($valuePattern.Current.IsReadOnly) {
            throw "The exact file-name edit unexpectedly reports IsReadOnly=true."
        }
    }
    else {
        $valuePattern = $null
    }
    $openInvoke = $null
    if ($null -ne $openElement -and
        $openElement.Current.ControlType -eq [Windows.Automation.ControlType]::Button -and
        $openElement.Current.ProcessId -eq $ProcessId) {
        $null = $openElement.TryGetCurrentPattern(
            [Windows.Automation.InvokePattern]::Pattern,
            [ref]$openInvoke
        )
    }
    $cancelInvoke = $null
    if ($null -ne $cancelElement -and
        $cancelElement.Current.ControlType -eq [Windows.Automation.ControlType]::Button -and
        $cancelElement.Current.ProcessId -eq $ProcessId) {
        $null = $cancelElement.TryGetCurrentPattern(
            [Windows.Automation.InvokePattern]::Pattern,
            [ref]$cancelInvoke
        )
    }
    $fileNameHostElement = $null
    $fileNameHostControls = @()
    if ($null -ne $fileNameHostRecord) {
        try {
            $fileNameHostElement = (
                [Windows.Automation.AutomationElement]::FromHandle(
                    [IntPtr]$fileNameHostRecord.handle
                )
            )
            $fileNameHostControls = @(
                $fileNameHostElement.FindAll(
                    [Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.Condition]::TrueCondition
                ) |
                    ForEach-Object { $_ } |
                    ForEach-Object {
                        $patterns = @(
                            $_.GetSupportedPatterns() |
                                ForEach-Object { $_.ProgrammaticName }
                        )
                        [ordered]@{
                            controlType = $_.Current.ControlType.ProgrammaticName
                            name = [string]$_.Current.Name
                            automationId = [string]$_.Current.AutomationId
                            className = [string]$_.Current.ClassName
                            runtimeId = Get-UiaRuntimeIdKey -Element $_
                            nativeWindowHandle = Get-WindowHandleHex `
                                -WindowHandle ([IntPtr]$_.Current.NativeWindowHandle)
                            patterns = $patterns
                        }
                    }
            )
        }
        catch {
            $fileNameHostControls = @()
        }
    }
    $dialogFileNameCandidates = @()
    if ($null -ne $fileNameElement) {
        $initialFileName = [string]$fileNameElement.Current.Name
        $dialogFileNameCandidates = @(
            $dialog.FindAll(
                [Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.Condition]::TrueCondition
            ) |
                ForEach-Object { $_ } |
                Where-Object {
                    ($initialFileName -ne "" -and
                        $_.Current.Name -eq $initialFileName) -or
                    $_.Current.AutomationId -eq "FileNameControlHost" -or
                    $_.Current.AutomationId -eq "1001"
                } |
                ForEach-Object {
                    $patterns = @(
                        $_.GetSupportedPatterns() |
                            ForEach-Object { $_.ProgrammaticName }
                    )
                    [ordered]@{
                        controlType = $_.Current.ControlType.ProgrammaticName
                        name = [string]$_.Current.Name
                        automationId = [string]$_.Current.AutomationId
                        className = [string]$_.Current.ClassName
                        runtimeId = Get-UiaRuntimeIdKey -Element $_
                        nativeWindowHandle = Get-WindowHandleHex `
                            -WindowHandle ([IntPtr]$_.Current.NativeWindowHandle)
                        patterns = $patterns
                    }
                }
        )
    }
    return [ordered]@{
        Dialog = $dialog
        DialogHandle = $DialogHandle
        Records = $records
        FileNameEdit = $fileNameElement
        FileNameEditRuntimeIdKey = if ($null -ne $fileNameElement) {
            Get-UiaRuntimeIdKey -Element $fileNameElement
        }
        else {
            $null
        }
        FileNameEditBounds = if ($null -ne $fileNameElement) {
            $fileNameElement.Current.BoundingRectangle
        }
        else {
            $null
        }
        FileNameValuePattern = $valuePattern
        FileNameHandle = [IntPtr]$fileNameRecord.handle
        FileNameHostHandle = [IntPtr]$fileNameHostRecord.handle
        FileNameHost = $fileNameHostElement
        FileNameHostRuntimeIdKey = if ($null -ne $fileNameHostElement) {
            Get-UiaRuntimeIdKey -Element $fileNameHostElement
        }
        else {
            $null
        }
        FileNameHostBounds = if ($null -ne $fileNameHostElement) {
            $fileNameHostElement.Current.BoundingRectangle
        }
        else {
            $null
        }
        OpenButton = $openElement
        OpenInvoke = $openInvoke
        OpenHandle = [IntPtr]$openRecord.handle
        CancelButton = $cancelElement
        CancelInvoke = $cancelInvoke
        CancelHandle = [IntPtr]$cancelRecord.handle
        Evidence = [ordered]@{
            dialogHandle = Get-WindowHandleHex -WindowHandle $DialogHandle
            dialogClassName = [string]$dialog.Current.ClassName
            resolutionAttempts = $attempts
            resolutionMilliseconds = $stopwatch.ElapsedMilliseconds
            fileNameControl = [ordered]@{
                source = $fileNameSource
                controlId = [int]$fileNameRecord.controlId
                className = [string]$fileNameRecord.className
                handle = [string]$fileNameRecord.handleText
                runtimeId = if ($null -ne $fileNameElement) {
                    Get-UiaRuntimeIdKey -Element $fileNameElement
                }
                else {
                    $null
                }
                bounds = if ($null -ne $fileNameElement) {
                    $bounds = $fileNameElement.Current.BoundingRectangle
                    [ordered]@{
                        x = $bounds.X
                        y = $bounds.Y
                        width = $bounds.Width
                        height = $bounds.Height
                    }
                }
                else {
                    $null
                }
                parentHandle = Get-WindowHandleHex -WindowHandle (
                    [IntPtr]$fileNameRecord.parentHandle
                )
                parentAutomationId = if ($null -ne $fileNameHostRecord) {
                    "FileNameControlHost"
                }
                else {
                    $null
                }
                hostRuntimeId = if ($null -ne $fileNameHostElement) {
                    Get-UiaRuntimeIdKey -Element $fileNameHostElement
                }
                else {
                    $null
                }
                hostBounds = if ($null -ne $fileNameHostElement) {
                    $bounds = $fileNameHostElement.Current.BoundingRectangle
                    [ordered]@{
                        x = $bounds.X
                        y = $bounds.Y
                        width = $bounds.Width
                        height = $bounds.Height
                    }
                }
                else {
                    $null
                }
                hostRawControls = $fileNameHostControls
                dialogFileNameCandidates = $dialogFileNameCandidates
                uiaControlType = if ($null -ne $fileNameElement) {
                    $fileNameElement.Current.ControlType.ProgrammaticName
                }
                else {
                    $null
                }
                inputPattern = if ($null -ne $valuePattern) {
                    "UIA ValuePattern"
                }
                else {
                    "unavailable: UIA Pane has no writable pattern"
                }
            }
            openControl = [ordered]@{
                controlId = 1
                className = [string]$openRecord.className
                handle = [string]$openRecord.handleText
                uiaControlType = if ($null -ne $openElement) {
                    $openElement.Current.ControlType.ProgrammaticName
                }
                else {
                    $null
                }
                invokePattern = if ($null -ne $openInvoke) {
                    "UIA InvokePattern"
                }
                else {
                    "exact HWND BM_CLICK"
                }
            }
            cancelControl = [ordered]@{
                controlId = 2
                className = [string]$cancelRecord.className
                handle = [string]$cancelRecord.handleText
                uiaControlType = if ($null -ne $cancelElement) {
                    $cancelElement.Current.ControlType.ProgrammaticName
                }
                else {
                    $null
                }
                invokePattern = if ($null -ne $cancelInvoke) {
                    "UIA InvokePattern"
                }
                else {
                    "exact HWND BM_CLICK"
                }
            }
        }
    }
}

function Close-NotepadFileDialog {
    param(
        [IntPtr]$DialogHandle,
        [object]$Anchors = $null
    )

    if ($DialogHandle -eq [IntPtr]::Zero -or
        -not [CortexComputerUseE2E.NativeMethods]::IsWindow($DialogHandle)) {
        return "file dialog already closed"
    }
    if ($null -ne $Anchors) {
        if ($null -ne $Anchors.CancelInvoke) {
            $Anchors.CancelInvoke.Invoke()
        }
        else {
            $null = [CortexComputerUseE2E.NativeMethods]::PostMessage(
                $Anchors.DialogHandle,
                0x0111,
                [IntPtr]::new(2),
                $Anchors.CancelHandle
            )
        }
    }
    else {
        $null = [CortexComputerUseE2E.NativeMethods]::PostMessage(
            $DialogHandle,
            0x0010,
            [IntPtr]::Zero,
            [IntPtr]::Zero
        )
    }
    $null = Wait-Until -TimeoutMilliseconds 5000 -FailureMessage (
        "The exact Notepad file dialog did not close safely."
    ) -Condition {
        return -not [CortexComputerUseE2E.NativeMethods]::IsWindow($DialogHandle)
    }
    return "closed exact file dialog $(Get-WindowHandleHex -WindowHandle $DialogHandle)"
}

function Invoke-NotepadCloseTestTabMenuItem {
    param([object]$Baseline)

    # Keep the exact tab selected so the File/Close Tab command can only apply
    # to the new test tab, even if another automation source changes focus.
    $newTab = Get-UiaElementByRuntimeId `
        -Root $Baseline.Root `
        -RuntimeId ([int[]]$script:PendingNotepadTestTabRuntimeId
        )
    $selection = [System.Windows.Automation.SelectionItemPattern]$newTab.GetCurrentPattern(
        [Windows.Automation.SelectionItemPattern]::Pattern
    )
    $selection.Select()

    $fileMenu = Find-UniqueUiaElement `
        -Scope $Baseline.Root `
        -ControlType ([Windows.Automation.ControlType]::MenuItem) `
        -AutomationId "File"
    $expandPattern = $null
    if (-not $fileMenu.TryGetCurrentPattern(
            [Windows.Automation.ExpandCollapsePattern]::Pattern,
            [ref]$expandPattern)) {
        throw "The exact Notepad File menu has no ExpandCollapsePattern."
    }
    $expand = [Windows.Automation.ExpandCollapsePattern]$expandPattern
    $expand.Expand()

    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        Start-Sleep -Milliseconds 100
        $menuItems = @(
            $Baseline.Root.FindAll(
                [Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::ControlTypeProperty,
                    [Windows.Automation.ControlType]::MenuItem
                )
            ) | ForEach-Object { $_ }
        )
        $closeItems = @(
            $menuItems | Where-Object {
                $_.Current.AcceleratorKey -eq "Ctrl+W" -and
                $_.Current.ClassName -eq "MenuFlyoutItem"
            }
        )
    } while ($closeItems.Count -ne 1 -and [DateTime]::UtcNow -lt $deadline)
    if ($closeItems.Count -ne 1) {
        try {
            $expand.Collapse()
        }
        catch {
        }
        throw (
            "Expected exactly one File menu item with AcceleratorKey 'Ctrl+W'; " +
            "found $($closeItems.Count)."
        )
    }

    $invoke = Get-NotepadInvokePattern `
        -Element $closeItems[0] `
        -Description "Notepad File/Close Tab menu item"
    $evidence = [ordered]@{
        fileMenuRuntimeId = Get-UiaRuntimeIdKey -Element $fileMenu
        closeItemName = [string]$closeItems[0].Current.Name
        closeItemAcceleratorKey = [string]$closeItems[0].Current.AcceleratorKey
        closeItemRuntimeId = Get-UiaRuntimeIdKey -Element $closeItems[0]
    }
    # Collapse prevents the popup bridge from surviving as a visible top-level
    # window and contaminating the later window-baseline restoration check.
    $expand.Collapse()
    Start-Sleep -Milliseconds 100
    $invoke.Invoke()
    return $evidence
}

function Confirm-NotepadDiscardTestTab {
    param([object]$Baseline)

    $buttons = @(
        $Baseline.Root.FindAll(
            [Windows.Automation.TreeScope]::Descendants,
            [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty,
                "SecondaryButton"
            )
        ) | ForEach-Object { $_ }
    )
    if ($buttons.Count -ne 1 -or
        $buttons[0].Current.ControlType -ne [Windows.Automation.ControlType]::Button -or
        $buttons[0].Current.Name -ne "不保存") {
        throw (
            "Expected exactly one Notepad Do-not-save SecondaryButton; found " +
            "$($buttons.Count)."
        )
    }
    $invoke = Get-NotepadInvokePattern `
        -Element $buttons[0] `
        -Description "Notepad Do-not-save button"
    $invoke.Invoke()
    return [ordered]@{
        automationId = "SecondaryButton"
        name = [string]$buttons[0].Current.Name
        runtimeId = Get-UiaRuntimeIdKey -Element $buttons[0]
    }
}

function Save-NotepadTestDocument {
    param(
        [object]$Baseline,
        [object]$TestTab,
        [string]$Path,
        [string]$Seed
    )

    $currentTabs = @(Get-NotepadTabSnapshot -Root $Baseline.Root)
    $newTabs = @(
        $currentTabs | Where-Object {
            $Baseline.Tabs.runtimeIdKey -notcontains $_.runtimeIdKey
        }
    )
    $selected = @($currentTabs | Where-Object { [bool]$_.selected })
    if ($currentTabs.Count -ne $Baseline.Tabs.Count + 1 -or
        $newTabs.Count -ne 1 -or
        $selected.Count -ne 1 -or
        [string]$newTabs[0].runtimeIdKey -ne $TestTab.TabRuntimeIdKey -or
        [string]$selected[0].runtimeIdKey -ne $TestTab.TabRuntimeIdKey) {
        throw "The test tab lost its exact unique-active identity before Open."
    }

    $document = Get-UiaElementByRuntimeId `
        -Root $Baseline.Root `
        -RuntimeId (
            [int[]](
                $TestTab.BlankDocumentRuntimeId.Split(".") |
                    ForEach-Object { [int]$_ }
            )
        )
    if ($document.Current.ControlType -ne [Windows.Automation.ControlType]::Document) {
        throw "The blank test document identity no longer resolves to a Document."
    }
    $dialogHandle = [IntPtr]::Zero
    $anchors = $null
    $readBack = $null
    $submitEvidence = $null
    try {
        $document.SetFocus()
        Send-KeysToOwnedWindow `
            -Process $Baseline.Process `
            -WindowHandle $Baseline.WindowHandle `
            -Keys "^s"
        $dialogHandle = Get-NeededForegroundWindow `
            -OriginalWindow $Baseline.WindowHandle `
            -ProcessId $Baseline.Process.Id
        $anchors = Get-NotepadFileDialogAnchors `
            -DialogHandle $dialogHandle `
            -ProcessId $Baseline.Process.Id

        if ($null -eq $anchors.FileNameValuePattern) {
            throw (
                "The exact FileNameControlHost/Edit is exposed only as a " +
                "non-writable UIA Pane; no safe reproducible filename mutation " +
                "was available, so Open was not submitted."
            )
        }
        $anchors.FileNameValuePattern.SetValue($Path)
        if (-not [CortexComputerUseE2E.NativeMethods]::IsWindow(
                $anchors.FileNameHandle
            ) -or
            [CortexComputerUseE2E.NativeMethods]::GetParent(
                $anchors.FileNameHandle
            ) -ne $anchors.FileNameHostHandle) {
            throw "The exact Save As file-name Edit lost its proven parent identity."
        }
        $readBack = [string]$anchors.FileNameValuePattern.Current.Value
        if ($readBack -ne $Path) {
            throw (
                "The exact Save As filename ValuePattern did not retain the " +
                "fixture path: expected '$Path', got '$readBack'."
            )
        }

        $submitEvidence = [ordered]@{
            method = $null
            dialogHandle = Get-WindowHandleHex -WindowHandle $anchors.DialogHandle
            openHandle = Get-WindowHandleHex -WindowHandle $anchors.OpenHandle
            fallbackMethod = $null
        }
        if ($null -ne $anchors.OpenInvoke) {
            $submitEvidence.method = "UIA InvokePattern"
            $anchors.OpenInvoke.Invoke()
        }
        else {
            # Common Dialog exposes the native OK button as a UIA Pane on this
            # build. The exact HWND/control identity is proven; post BM_CLICK
            # without blocking this STA thread on the dialog thread. A direct
            # WM_COMMAND/IDOK can dismiss the shell dialog without running the
            # button's Save handler, so it is only a fallback.
            if (-not [CortexComputerUseE2E.NativeMethods]::IsWindow(
                    $anchors.DialogHandle
                ) -or
                -not [CortexComputerUseE2E.NativeMethods]::IsWindow(
                    $anchors.OpenHandle
                ) -or
                [CortexComputerUseE2E.NativeMethods]::GetDlgCtrlID(
                    $anchors.OpenHandle
                ) -ne 1 -or
                [CortexComputerUseE2E.NativeMethods]::GetWindowClassName(
                    $anchors.OpenHandle
                ) -ne "Button") {
                throw "The exact Button/1 identity changed before Save As submission."
            }
            if (-not [CortexComputerUseE2E.NativeMethods]::PostMessage(
                    $anchors.OpenHandle,
                    0x00F5,
                    [IntPtr]::Zero,
                    [IntPtr]::Zero
                )) {
                throw (
                    "PostMessage(BM_CLICK) failed for the exact Save As " +
                    "Button/1: " +
                    [Runtime.InteropServices.Marshal]::GetLastWin32Error()
                )
            }
            $submitEvidence.method = "exact HWND PostMessage BM_CLICK"
            Start-Sleep -Milliseconds 1000
            if ([CortexComputerUseE2E.NativeMethods]::IsWindow(
                    $anchors.DialogHandle
                )) {
                if (-not [CortexComputerUseE2E.NativeMethods]::IsWindow(
                        $anchors.OpenHandle
                    ) -or
                    [CortexComputerUseE2E.NativeMethods]::GetDlgCtrlID(
                        $anchors.OpenHandle
                    ) -ne 1 -or
                    [CortexComputerUseE2E.NativeMethods]::GetWindowClassName(
                        $anchors.OpenHandle
                    ) -ne "Button") {
                    throw (
                        "The exact Save As identity changed before the fallback " +
                        "WM_COMMAND submission."
                    )
                }
                if (-not [CortexComputerUseE2E.NativeMethods]::PostMessage(
                        $anchors.DialogHandle,
                        0x0111,
                        [IntPtr]::new(1),
                        $anchors.OpenHandle
                    )) {
                    throw (
                        "PostMessage(WM_COMMAND IDOK) failed for the exact Save " +
                        "As dialog: " +
                        [Runtime.InteropServices.Marshal]::GetLastWin32Error()
                    )
                }
                $submitEvidence.fallbackMethod = (
                    "exact HWND PostMessage WM_COMMAND/IDOK"
                )
            }
        }

        $null = Wait-Until -TimeoutMilliseconds 10000 -FailureMessage (
            "The exact Save As dialog did not close after submitting the fixture path."
        ) -Condition {
            return -not [CortexComputerUseE2E.NativeMethods]::IsWindow($dialogHandle)
        }
        $currentTabs = @(Get-NotepadTabSnapshot -Root $Baseline.Root)
        $selected = @($currentTabs | Where-Object { [bool]$_.selected })
        if ($currentTabs.Count -ne $Baseline.Tabs.Count + 1 -or
            $selected.Count -ne 1 -or
            [string]$selected[0].runtimeIdKey -ne $TestTab.TabRuntimeIdKey) {
            throw (
                "The exact new tab did not remain uniquely active after Save As; " +
                "tabCount=$($currentTabs.Count), selectedCount=$($selected.Count)."
            )
        }
        $documentAfterSave = Get-UiaElementByRuntimeId `
            -Root $Baseline.Root `
            -RuntimeId (
                [int[]](
                    $TestTab.BlankDocumentRuntimeId.Split(".") |
                        ForEach-Object { [int]$_ }
                )
            )
        if ($documentAfterSave.Current.ControlType -ne
            [Windows.Automation.ControlType]::Document) {
            throw "The exact new-tab Document identity did not survive Save As."
        }
        $null = Wait-Until -TimeoutMilliseconds 10000 -FailureMessage (
            "Notepad did not write the exact fixture file after Save As."
        ) -Condition {
            return (Test-Path -LiteralPath $Path -PathType Leaf) -and
                ([System.IO.File]::ReadAllText($Path) -eq $Seed)
        }

        return [ordered]@{
            ok = $true
            Evidence = [ordered]@{
                dialog = $anchors.Evidence
                submission = $submitEvidence
                fileNameReadBack = $readBack
                diskText = [System.IO.File]::ReadAllText($Path)
            }
        }
    }
    catch {
        $dialogCleanup = $null
        try {
            $dialogCleanup = Close-NotepadFileDialog `
                -DialogHandle $dialogHandle `
                -Anchors $anchors
        }
        catch {
            $dialogCleanup = "Save As cleanup failed: $($_.Exception.Message)"
        }
        return [ordered]@{
            ok = $false
            reason = "The exact Save As path was not uniquely provable: " +
                $_.Exception.Message
            details = [ordered]@{
                fileDialog = if ($null -ne $anchors) {
                    $anchors.Evidence
                }
                else {
                    $null
                }
                fixturePath = $Path
                fileNameReadBack = $readBack
                submission = $submitEvidence
                dialogStillOpen = (
                    $dialogHandle -ne [IntPtr]::Zero -and
                    [CortexComputerUseE2E.NativeMethods]::IsWindow($dialogHandle)
                )
                fixtureExists = Test-Path -LiteralPath $Path -PathType Leaf
                cleanup = $dialogCleanup
            }
        }
    }
}

function Close-NotepadTestTab {
    param(
        [object]$Baseline,
        [object]$TestTab,
        [object]$State
    )

    $tabsBeforeClose = @(Get-NotepadTabSnapshot -Root $Baseline.Root)
    $testTabs = @(
        $tabsBeforeClose | Where-Object {
            [string]$_.runtimeIdKey -eq $TestTab.TabRuntimeIdKey
        }
    )
    $selectedBeforeClose = @(
        $tabsBeforeClose | Where-Object { [bool]$_.selected }
    )
    if ($tabsBeforeClose.Count -ne $Baseline.Tabs.Count + 1 -or
        $testTabs.Count -ne 1 -or
        $selectedBeforeClose.Count -ne 1 -or
        [string]$selectedBeforeClose[0].runtimeIdKey -ne $TestTab.TabRuntimeIdKey) {
        throw "The exact test tab is not the unique active tab before close."
    }

    $menuEvidence = Invoke-NotepadCloseTestTabMenuItem -Baseline $Baseline
    $discardEvidence = $null
    $deadline = [DateTime]::UtcNow.AddSeconds(3)
    do {
        Start-Sleep -Milliseconds 100
        $secondaryButtons = @(
            $Baseline.Root.FindAll(
                [Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty,
                    "SecondaryButton"
                )
            ) | ForEach-Object { $_ }
        )
    } while ($secondaryButtons.Count -lt 1 -and [DateTime]::UtcNow -lt $deadline)
    if ($secondaryButtons.Count -gt 0) {
        $discardEvidence = Confirm-NotepadDiscardTestTab -Baseline $Baseline
    }

    $deadline = [DateTime]::UtcNow.AddSeconds(8)
    do {
        Start-Sleep -Milliseconds 100
        $tabsAfterClose = @(Get-NotepadTabSnapshot -Root $Baseline.Root)
        # 关闭标签后 Notepad 按自己的历史选择邻近标签；恢复操作前的活动标签，
        # 不能把“标签都还在”误当成“用户原来的活动文档也已恢复”。
        if ($tabsAfterClose.Count -eq $Baseline.Tabs.Count -and
            @($tabsAfterClose | Where-Object {
                $_.runtimeIdKey -eq $TestTab.TabRuntimeIdKey
            }).Count -eq 0) {
            $originalSelected = @($Baseline.Tabs | Where-Object { $_.selected })
            if ($originalSelected.Count -eq 1 -and
                @($tabsAfterClose | Where-Object {
                    $_.selected -and $_.runtimeIdKey -eq $originalSelected[0].runtimeIdKey
                }).Count -eq 0) {
                $originalTab = Get-UiaElementByRuntimeId -Root $Baseline.Root `
                    -RuntimeId ([int[]]$originalSelected[0].runtimeId)
                (Get-SelectionItemPattern -Element $originalTab).Select()
                $tabsAfterClose = @(Get-NotepadTabSnapshot -Root $Baseline.Root)
            }
        }
        $documentsAfterClose = @(Get-NotepadVisibleDocuments -Root $Baseline.Root)
    } while ((
            -not (Test-NotepadTabSnapshotsEqual `
                -Expected $Baseline.Tabs `
                -Actual $tabsAfterClose) -or
            $documentsAfterClose.Count -ne 1 -or
            (Get-UiaRuntimeIdKey -Element $documentsAfterClose[0]) -ne
                [string]$Baseline.Evidence.baselineDocumentRuntimeId
        ) -and [DateTime]::UtcNow -lt $deadline)

    if (-not (Test-NotepadTabSnapshotsEqual `
            -Expected $Baseline.Tabs `
            -Actual $tabsAfterClose)) {
        throw "Closing the test tab did not restore the exact baseline tab snapshot."
    }
    if ($documentsAfterClose.Count -ne 1 -or
        (Get-UiaRuntimeIdKey -Element $documentsAfterClose[0]) -ne
            [string]$Baseline.Evidence.baselineDocumentRuntimeId) {
        throw "Closing the test tab did not restore the exact baseline active Document."
    }

    $currentApp = Get-AppWindowSnapshot -ProcessNames @("notepad")
    if ((@($currentApp.processIds) -join ",") -ne
            (@($Baseline.AppSnapshot.processIds) -join ",") -or
        ((@($currentApp.windows) | ForEach-Object {
                    "$($_.processId):$($_.handle):$($_.title)"
                } | Sort-Object) -join "|") -ne
            ((@($Baseline.AppSnapshot.windows) | ForEach-Object {
                        "$($_.processId):$($_.handle):$($_.title)"
                    } | Sort-Object) -join "|")) {
        throw "Closing the test tab changed the user's Notepad PID/HWND/window set."
    }
    $null = Assert-AppWindowsPreserved -Snapshot $Baseline.AppSnapshot

    return [ordered]@{
        cleanup = "closed exact new TabItem through File/Close Tab and restored baseline tabs"
        Evidence = [ordered]@{
                closeMenuItem = $menuEvidence
                discardButton = $discardEvidence
                tabRuntimeId = $TestTab.TabRuntimeIdKey
            tabsAfterClose = $tabsAfterClose
            activeDocumentRuntimeId = Get-UiaRuntimeIdKey -Element $documentsAfterClose[0]
            userWindowPreserved = $true
            userTabsPreserved = $true
        }
    }
}

function Set-EditorText {
    param(
        [System.Diagnostics.Process]$Process,
        [IntPtr]$WindowHandle,
        [Windows.Automation.AutomationElement]$Editor,
        [string]$Text
    )

    $pattern = $null
    if ($Editor.TryGetCurrentPattern(
            [Windows.Automation.ValuePattern]::Pattern,
            [ref]$pattern)) {
        $valuePattern = [Windows.Automation.ValuePattern]$pattern
        if (-not $valuePattern.Current.IsReadOnly) {
            $valuePattern.SetValue($Text)
            return "ValuePattern"
        }
    }

    Assert-ForegroundOwnership -Process $Process -WindowHandle $WindowHandle
    $Editor.SetFocus()
    [System.Windows.Forms.SendKeys]::SendWait("^a")
    [System.Windows.Forms.SendKeys]::SendWait($Text)
    return "keyboard after foreground ownership proof"
}

function Start-NotepadForFile {
    param(
        [string]$Path,
        [object]$BaselineSnapshot,
        [string]$IsolationArgument = ""
    )

    if ([string]::IsNullOrWhiteSpace($script:NotepadLauncherExe)) {
        throw "Windows Notepad was not found."
    }

    $baselineHandles = @(
        $BaselineSnapshot.windows |
            ForEach-Object { [string]$_.handle.ToUpperInvariant() }
    )
    $baselineProcessIds = @(
        $BaselineSnapshot.processIds |
            ForEach-Object { [int]$_ }
    )
    $arguments = if ([string]::IsNullOrWhiteSpace($IsolationArgument)) {
        Quote-ProcessArgument $Path
    }
    else {
        "$IsolationArgument $(Quote-ProcessArgument $Path)"
    }
    $launcher = Start-ExactProcess -FilePath $script:NotepadLauncherExe `
        -Arguments $arguments

    $fileName = [System.IO.Path]::GetFileName($Path)
    $titlePattern = "^{0}(?:\s+-\s+.*)?$" -f [regex]::Escape($fileName)
    $deadline = [DateTime]::UtcNow.AddSeconds(18)
    while ([DateTime]::UtcNow -lt $deadline) {
        $current = Get-AppWindowSnapshot -ProcessNames @("notepad")
        $newWindows = @(
            $current.windows |
                Where-Object {
                    $baselineHandles -notcontains
                        ([string]$_.handle).ToUpperInvariant()
                }
        )
        if ($newWindows.Count -gt 1) {
            throw (
                "Notepad created $($newWindows.Count) new top-level windows; the " +
                "fixture cannot choose an exact target safely."
            )
        }
        if ($newWindows.Count -eq 1 -and
            ([string]$newWindows[0].title) -match $titlePattern) {
            $process = Get-Process `
                -Id ([int]$newWindows[0].processId) `
                -ErrorAction Stop
            $null = Assert-ExactProcessPath `
                -Process $process `
                -Allowlist $script:NotepadAllowlist
            $handle = Convert-WindowHandle `
                -WindowHandle ([string]$newWindows[0].handle)
            $ownerProcessId = [uint32]0
            $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
                $handle,
                [ref]$ownerProcessId
            )
            if ([int]$ownerProcessId -ne $process.Id) {
                throw (
                    "The new Notepad window belongs to pid $ownerProcessId, not " +
                    "the enumerated pid $($process.Id)."
                )
            }
            $windowsPreserved = Assert-AppWindowsPreserved -Snapshot $BaselineSnapshot
            if (-not $windowsPreserved) {
                throw "Pre-existing Notepad windows were not preserved."
            }
            return [ordered]@{
                Process = $process
                Launcher = $launcher
                WindowHandle = $handle
                Evidence = [ordered]@{
                    processId = $process.Id
                    windowId = Get-WindowHandleHex -WindowHandle $handle
                    windowProcessId = [int]$ownerProcessId
                    windowTitle = [string]$newWindows[0].title
                    windowNotInBaseline = $true
                    processWasPreExisting = $baselineProcessIds -contains $process.Id
                    launchArgument = $IsolationArgument
                    commandLine = Get-ProcessCommandLine -ProcessId $process.Id
                    preExistingProcessIds = @($baselineProcessIds)
                    preExistingWindows = @($BaselineSnapshot.windows)
                    preExistingWindowsPreserved = $true
                }
            }
        }
        Start-Sleep -Milliseconds 100
    }

    $launcher.Refresh()
    if (-not $launcher.HasExited) {
        $null = Stop-TestProcess -Process $launcher
    }
    throw (
        "Windows Notepad did not create exactly one new top-level window for " +
        "the acceptance file; no UI input was sent."
    )
}

function Close-NotepadTestWindow {
    param(
        [System.Diagnostics.Process]$Process,
        [IntPtr]$WindowHandle,
        [object]$BaselineSnapshot
    )

    if ($WindowHandle -eq [IntPtr]::Zero -or
        -not [CortexComputerUseE2E.NativeMethods]::IsWindow($WindowHandle)) {
        return "isolated test window already closed"
    }
    $baselineProcessIds = @(
        $BaselineSnapshot.processIds |
            ForEach-Object { [int]$_ }
    )
    if ($baselineProcessIds -notcontains $Process.Id) {
        return Stop-TestProcess -Process $Process
    }

    $handleText = Get-WindowHandleHex -WindowHandle $WindowHandle
    if (-not [CortexComputerUseE2E.NativeMethods]::PostMessage(
            $WindowHandle,
            0x0010,
            [IntPtr]::Zero,
            [IntPtr]::Zero)) {
        throw "WM_CLOSE could not be posted to the exact test window $handleText."
    }
    $null = Wait-Until -TimeoutMilliseconds 5000 -FailureMessage (
        "The exact test window $handleText did not close; its process was left running."
    ) -Condition {
        return -not [CortexComputerUseE2E.NativeMethods]::IsWindow($WindowHandle)
    }
    $Process.Dispose()
    return "posted WM_CLOSE only to exact test window $handleText"
}

function Invoke-NotepadAction {
    param(
        [string]$TaskId,
        [string[]]$Allowlist
    )

    if ([string]::IsNullOrWhiteSpace($script:NotepadLauncherExe)) {
        return New-SkipResult -Reason "Windows Notepad is not installed."
    }
    # Keep the temporary directory until task completion. The direct-tab
    # tasks never create a fixture file, and Save As is skipped before it
    # can write an unverified path.
    $directory = New-TestDirectory -Purpose "notepad"
    $path = Join-Path $directory "acceptance.txt"
    $seed = "Notepad seed $([guid]::NewGuid().ToString('N'))"
    $originalAppSnapshot = Get-AppWindowSnapshot -ProcessNames @("notepad")
    $ownedBaseline = $null
    $tabBaseline = $null
    $testTab = $null
    $saveEvidence = $null
    $result = $null
    $closeEvidence = $null
    $baselineCloseEvidence = $null
    try {
        # E2E workers run in isolated Job Objects and must be deterministic on a
        # clean desktop. If Notepad is not already running, open a dedicated,
        # read-only baseline file and remove the whole instance during cleanup.
        # When a user instance exists, preserve its PIDs, tabs and documents.
        $appBaseline = $originalAppSnapshot
        if ($appBaseline.windows.Count -eq 0) {
            $baselinePath = Join-Path $directory "baseline.txt"
            [System.IO.File]::WriteAllText(
                $baselinePath,
                "Cortex Computer Use isolated Notepad baseline.",
                [System.Text.UTF8Encoding]::new($false)
            )
            $ownedBaseline = Start-NotepadForFile `
                -Path $baselinePath `
                -BaselineSnapshot $appBaseline
            $appBaseline = Wait-Until -TimeoutMilliseconds 8000 `
                -FailureMessage (
                    "The isolated Notepad baseline did not settle to one exact " +
                    "process and window."
                ) -Condition {
                    $candidate = Get-AppWindowSnapshot -ProcessNames @("notepad")
                    if ($candidate.windows.Count -ne 1) {
                        return $false
                    }
                    return $candidate
                }
        }

        $tabBaseline = New-NotepadTabBaseline `
            -AppSnapshot $appBaseline `
            -Allowlist $Allowlist
        if (-not [bool]$tabBaseline.ok) {
            # A failed close can briefly leave transient menu-popup windows. Wait
            # for the real Notepad window set to settle before deciding to skip.
            $settled = $null
            $null = Wait-Until -TimeoutMilliseconds 5000 -FailureMessage (
                "Notepad's visible top-level windows did not settle."
            ) -Condition {
                $settled = Get-AppWindowSnapshot -ProcessNames @("notepad")
                return $settled.windows.Count -eq 1
            }
            $tabBaseline = New-NotepadTabBaseline `
                -AppSnapshot $settled `
                -Allowlist $Allowlist
        }
        if (-not [bool]$tabBaseline.ok) {
            $result = New-SkipResult `
                -Reason $tabBaseline.reason `
                -Details ([ordered]@{
                    decision = "skip"
                    addButtonInputSent = $false
                    documentInputSent = $false
                    baseline = $tabBaseline.details
                    baselineOwnedByHarness = $null -ne $ownedBaseline
                })
        }
        else {
        $testTab = New-NotepadTestTab -Baseline $tabBaseline
        if (-not [bool]$testTab.ok) {
            $result = New-SkipResult `
                -Reason $testTab.reason `
                -Details ([ordered]@{
                    decision = "skip"
                    addButtonInputSent = $false
                    documentInputSent = $false
                    tabIsolation = $testTab.details
                })
        }
        else {
            $script:PendingNotepadTestTabRuntimeId = $testTab.TabRuntimeId
            $document = Get-UiaElementByRuntimeId `
                -Root $tabBaseline.Root `
                -RuntimeId (
                    [int[]](
                        $testTab.BlankDocumentRuntimeId.Split(".") |
                            ForEach-Object { [int]$_ }
                    )
                )
            if ($document.Current.ControlType -ne
                [Windows.Automation.ControlType]::Document) {
                throw "The new test-document RuntimeId is no longer a Document."
            }

            $delivery = Set-EditorText `
                -Process $tabBaseline.Process `
                -WindowHandle $tabBaseline.WindowHandle `
                -Editor $document `
                -Text $seed
            $null = Wait-Until -TimeoutMilliseconds 8000 -FailureMessage (
                "The exact new-tab Document did not show the seed text."
            ) -Condition {
                return (Get-ControlText -Element $document) -eq $seed
            }

            if ($TaskId -eq "notepad-open-read") {
                $actual = Get-ControlText -Element $document
                $result = New-PassResult `
                    -Kind "Notepad exact new-tab UIA text read" `
                    -Expected $seed `
                    -Actual $actual `
                    -Details ([ordered]@{
                        delivery = $delivery
                        tabSession = [ordered]@{
                            baseline = $tabBaseline.Evidence
                            newTab = $testTab.Evidence
                        }
                    })
            }
            elseif ($TaskId -eq "notepad-replace-text") {
                $expected = "Notepad replacement $([guid]::NewGuid().ToString('N'))"
                $replaceDelivery = Set-EditorText `
                    -Process $tabBaseline.Process `
                    -WindowHandle $tabBaseline.WindowHandle `
                    -Editor $document `
                    -Text $expected
                $null = Wait-Until -TimeoutMilliseconds 8000 -FailureMessage (
                    "The exact new-tab Document did not show the replacement text."
                ) -Condition {
                    return (Get-ControlText -Element $document) -eq $expected
                }
                $actual = Get-ControlText -Element $document
                $result = New-PassResult `
                    -Kind "Notepad exact active-tab text replacement" `
                    -Expected $expected `
                    -Actual $actual `
                    -Details ([ordered]@{
                        seedDelivery = $delivery
                        delivery = $replaceDelivery
                        tabSession = [ordered]@{
                            baseline = $tabBaseline.Evidence
                            newTab = $testTab.Evidence
                        }
                    })
            }
            elseif ($TaskId -eq "notepad-save-file") {
                $saveEvidence = Save-NotepadTestDocument `
                    -Baseline $tabBaseline `
                    -TestTab $testTab `
                    -Path $path `
                    -Seed $seed
                if (-not [bool]$saveEvidence.ok) {
                    $result = New-SkipResult `
                        -Reason $saveEvidence.reason `
                        -Details ([ordered]@{
                            decision = "skip"
                            addButtonInputSent = $true
                            documentInputSent = $true
                            saveDialog = $saveEvidence.details
                            tabSession = [ordered]@{
                                baseline = $tabBaseline.Evidence
                                newTab = $testTab.Evidence
                            }
                        })
                }
                else {
                    $actual = [System.IO.File]::ReadAllText($path)
                    $result = New-PassResult `
                        -Kind "Notepad exact active-tab edit plus filesystem verifier" `
                        -Expected $seed `
                        -Actual $actual `
                        -Details ([ordered]@{
                            file = $path
                            seedDelivery = $delivery
                            saveDialog = $saveEvidence.Evidence
                            tabSession = [ordered]@{
                                baseline = $tabBaseline.Evidence
                                newTab = $testTab.Evidence
                            }
                        })
                }
            }
            else {
                throw "Unknown Notepad acceptance task: $TaskId"
            }
        }
        }
    }
    finally {
        if ($null -ne $testTab -and [bool]$testTab.ok) {
            $closeEvidence = Close-NotepadTestTab `
                -Baseline $tabBaseline `
                -TestTab $testTab `
                -State $saveEvidence
            $script:PendingNotepadTestTabRuntimeId = $null
        }
        if ($null -ne $ownedBaseline) {
            $baselineCloseEvidence = Close-NotepadTestWindow `
                -Process $ownedBaseline.Process `
                -WindowHandle $ownedBaseline.WindowHandle `
                -BaselineSnapshot $originalAppSnapshot
        }
        Remove-TestDirectory -Path $directory
    }
    if ($null -eq $result) {
        throw "Notepad acceptance task '$TaskId' produced no result."
    }
    if (-not [string]::IsNullOrWhiteSpace([string]$closeEvidence.cleanup)) {
        Add-CleanupEvidence `
            -Result $result `
            -Evidence ([string]$closeEvidence.cleanup)
    }
    if (-not [string]::IsNullOrWhiteSpace([string]$baselineCloseEvidence)) {
        Add-CleanupEvidence `
            -Result $result `
            -Evidence ([string]$baselineCloseEvidence)
    }
    if ($null -ne $result.details -and $null -ne $closeEvidence) {
        $result.details.baselineRestoration = $closeEvidence.Evidence
    }
    return $result
}

function Start-IsolatedExplorer {
    param([string]$Folder)

    if ([string]::IsNullOrWhiteSpace($script:ExplorerExe)) {
        throw "Windows Explorer was not found."
    }

    $before = @(
        Get-Process -Name explorer -ErrorAction SilentlyContinue |
            Select-Object -ExpandProperty Id
    )
    $process = Start-ExactProcess -FilePath $script:ExplorerExe `
        -Arguments ("/separate," + (Quote-ProcessArgument $Folder))

    $newProcess = Wait-Until -TimeoutMilliseconds 8000 -FailureMessage (
        "Explorer /separate did not create a distinct process for isolated acceptance."
    ) -Condition {
        $candidate = Get-Process -Name explorer -ErrorAction SilentlyContinue |
            Where-Object { $before -notcontains $_.Id } |
            Sort-Object StartTime -Descending |
            Select-Object -First 1
        if ($null -eq $candidate) {
            return $false
        }
        $candidate.Refresh()
        if ($candidate.HasExited -or $candidate.MainWindowHandle -eq [IntPtr]::Zero) {
            return $false
        }
        return $candidate
    }
    if ($process.Id -ne $newProcess.Id) {
        $null = Stop-TestProcess -Process $process
    }
    return $newProcess
}

function Invoke-ExplorerAction {
    param(
        [string]$TaskId,
        [string[]]$Allowlist
    )

    if ([string]::IsNullOrWhiteSpace($script:ExplorerExe)) {
        return New-SkipResult -Reason "Windows Explorer is not installed."
    }

    $directory = New-TestDirectory -Purpose "explorer"
    $folderName = "Cortex E2E $([guid]::NewGuid().ToString('N'))"
    $folder = Join-Path $directory $folderName
    $null = New-Item -ItemType Directory -Path $folder
    $fileName = "acceptance-$([guid]::NewGuid().ToString('N')).txt"
    $filePath = Join-Path $folder $fileName
    [System.IO.File]::WriteAllText($filePath, "Explorer acceptance", [System.Text.UTF8Encoding]::new($false))
    $process = $null
    try {
        try {
            $process = Start-IsolatedExplorer -Folder $folder
        }
        catch {
            return New-SkipResult `
                -Reason ("Explorer reused a shared shell process, so the task cannot prove " +
                    "that no unrelated Explorer window is touched.") `
                -Details @{ error = $_.Exception.Message }
        }

        $handle = Wait-MainWindow -Process $process -TimeoutMilliseconds 15000
        $root = Get-WindowRoot -Process $process -Allowlist $Allowlist
        $item = Wait-Until -TimeoutMilliseconds 12000 -FailureMessage (
            "Explorer did not expose the exact temporary file in the isolated folder window."
        ) -Condition {
            return Find-ByName -Root $root -Name $fileName
        }

        if ($TaskId -eq "explorer-list-folder") {
            return New-PassResult -Kind "Explorer isolated folder item discovery" `
                -Expected @{
                    folderName = $folderName
                    fileName = $fileName
                    processId = $process.Id
                } -Actual @{
                    folderName = $root.Current.Name
                    fileName = $item.Current.Name
                    processId = $root.Current.ProcessId
                } -Details @{ file = $filePath }
        }

        if ($TaskId -eq "explorer-select-file") {
            $pattern = Get-SelectionItemPattern -Element $item
            $pattern.Select()
            $null = Wait-Until -TimeoutMilliseconds 5000 -FailureMessage (
                "Explorer file item did not report SelectionItemPattern.IsSelected=true."
            ) -Condition {
                return $pattern.Current.IsSelected
            }
            return New-PassResult -Kind "Explorer SelectionItemPattern selection" `
                -Expected @{ fileName = $fileName; selected = $true } `
                -Actual @{
                    fileName = $item.Current.Name
                    selected = $pattern.Current.IsSelected
                    processId = $process.Id
                } -Details @{ file = $filePath }
        }

        throw "Unknown Explorer acceptance task: $TaskId"
    }
    finally {
        if ($null -ne $process) {
            $null = Stop-TestProcess -Process $process
        }
        Remove-TestDirectory -Path $directory
    }
}

function New-SeedWorkbook {
    param([string]$Path)

    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::Open(
        $Path,
        [System.IO.Compression.ZipArchiveMode]::Create
    )
    try {
        $entries = [ordered]@{
            "[Content_Types].xml" = @"
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>
"@
            "_rels/.rels" = @"
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>
"@
            "xl/workbook.xml" = @"
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Acceptance" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>
"@
            "xl/_rels/workbook.xml.rels" = @"
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>
"@
            "xl/worksheets/sheet1.xml" = @"
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="inlineStr"><is><t>Excel acceptance seed</t></is></c>
    </row>
  </sheetData>
</worksheet>
"@
        }
        foreach ($name in $entries.Keys) {
            $entry = $archive.CreateEntry($name)
            $stream = $entry.Open()
            $writer = [System.IO.StreamWriter]::new(
                $stream,
                [System.Text.UTF8Encoding]::new($false)
            )
            try {
                $writer.Write($entries[$name])
            }
            finally {
                $writer.Dispose()
                $stream.Dispose()
            }
        }
    }
    finally {
        $archive.Dispose()
    }
}

function Read-ExcelCell {
    param(
        [string]$Path,
        [string]$Cell
    )

    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($Path)
    try {
        $sheetEntry = $archive.GetEntry("xl/worksheets/sheet1.xml")
        if ($null -eq $sheetEntry) {
            throw "Workbook has no xl/worksheets/sheet1.xml."
        }
        $reader = [System.IO.StreamReader]::new($sheetEntry.Open())
        try {
            $sheetXml = [xml]$reader.ReadToEnd()
        }
        finally {
            $reader.Dispose()
        }

        $sharedStrings = @()
        $sharedEntry = $archive.GetEntry("xl/sharedStrings.xml")
        if ($null -ne $sharedEntry) {
            $sharedReader = [System.IO.StreamReader]::new($sharedEntry.Open())
            try {
                $sharedXml = [xml]$sharedReader.ReadToEnd()
            }
            finally {
                $sharedReader.Dispose()
            }
            foreach ($item in $sharedXml.SelectNodes("//*[local-name()='si']")) {
                $sharedStrings += (($item.SelectNodes(".//*[local-name()='t']") |
                    ForEach-Object { $_.InnerText }) -join "")
            }
        }

        $cellNode = $sheetXml.SelectSingleNode(
            "//*[local-name()='c' and @r='$Cell']"
        )
        if ($null -eq $cellNode) {
            return $null
        }
        $type = $cellNode.GetAttribute("t")
        if ($type -eq "inlineStr") {
            return (($cellNode.SelectNodes(".//*[local-name()='t']") |
                ForEach-Object { $_.InnerText }) -join "")
        }
        $valueNode = $cellNode.SelectSingleNode("./*[local-name()='v']")
        if ($null -eq $valueNode) {
            return $null
        }
        if ($type -eq "s") {
            $index = [int]$valueNode.InnerText
            if ($index -lt 0 -or $index -ge $sharedStrings.Count) {
                throw "Workbook shared string index $index is out of range."
            }
            return [string]$sharedStrings[$index]
        }
        return $valueNode.InnerText
    }
    finally {
        $archive.Dispose()
    }
}

function Start-ExcelForWorkbook {
    param([string]$Path)

    return Start-ExactProcess -FilePath $script:ExcelExe `
        -Arguments ("/x " + (Quote-ProcessArgument $Path))
}

function Invoke-ExcelAction {
    param(
        [string]$TaskId,
        [string[]]$Allowlist
    )

    if ([string]::IsNullOrWhiteSpace($script:ExcelExe)) {
        return New-SkipResult -Reason "Microsoft Excel is not installed."
    }
    $existingExcelProcesses = @(
        Get-Process -Name EXCEL -ErrorAction SilentlyContinue |
            Where-Object {
                $_.Refresh()
                -not $_.HasExited
            }
    )
    if ($existingExcelProcesses.Count -gt 0) {
        return New-SkipResult `
            -Reason ("An existing Excel or Office COM process is running. The suite will " +
                "not attach to a shared Office instance because it cannot prove that no " +
                "unrelated workbook or automation session is touched.") `
            -Details @{
                existingProcessIds = @($existingExcelProcesses | ForEach-Object { $_.Id })
            }
    }
    if ($TaskId -eq "excel-edit-save-cell" -and
        (Test-WindowsSessionLocked)) {
        return New-SkipResult `
            -Reason ("The Windows desktop is locked, so the suite cannot " +
                "prove foreground keyboard delivery to the exact Excel edit " +
                "window. Unlock the session and rerun this acceptance.") `
            -Details @{
                decision = "skip"
                keyboardInputSent = $false
                desktopLocked = $true
            }
    }

    $directory = New-TestDirectory -Purpose "excel"
    $path = Join-Path $directory "acceptance.xlsx"
    try {
        New-SeedWorkbook -Path $path
    }
    catch {
        Remove-TestDirectory -Path $directory
        return New-SkipResult `
            -Reason "The OpenXML workbook fixture could not be generated." `
            -Details @{ error = $_.Exception.Message }
    }
    $seedA1 = Read-ExcelCell -Path $path -Cell "A1"
    if ($seedA1 -ne "Excel acceptance seed") {
        Remove-TestDirectory -Path $directory
        throw "Generated workbook A1 mismatch: '$seedA1'."
    }
    $seedHash = Get-Sha256File -Path $path

    $process = $null
    try {
        $process = Start-ExcelForWorkbook -Path $path
        $handle = Wait-MainWindow -Process $process -TimeoutMilliseconds 30000 `
            -TitlePattern "acceptance\.xlsx"
        $root = Get-WindowRoot -Process $process -Allowlist $Allowlist
        $windowOwnerProcessId = [uint32]0
        $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
            $handle,
            [ref]$windowOwnerProcessId
        )
        if ([int]$windowOwnerProcessId -ne $process.Id) {
            throw (
                "Excel acceptance window belongs to pid $windowOwnerProcessId, not " +
                "the exact launched pid $($process.Id)."
            )
        }
        $windowEvidence = [ordered]@{
            processId = $process.Id
            windowId = (Get-WindowHandleHex -WindowHandle $handle)
            windowProcessId = [int]$windowOwnerProcessId
            commandLine = (Get-ProcessCommandLine -ProcessId $process.Id)
        }
        Write-E2EDebugTree -Root $root -Label "excel"

        if ($TaskId -eq "excel-open-workbook") {
            return New-PassResult -Kind "Excel isolated workbook launch and OpenXML verifier" `
                -Expected @{
                    a1 = "Excel acceptance seed"
                    seedSha256 = $seedHash
                    processId = $process.Id
                } -Actual @{
                    a1 = $seedA1
                    seedSha256 = $seedHash
                    processId = $root.Current.ProcessId
                    windowTitle = $root.Current.Name
                } -Details @{
                    workbook = $path
                    isolation = $windowEvidence
                }
        }

        if ($TaskId -eq "excel-edit-save-cell") {
            $expected = "cortexexcel$([guid]::NewGuid().ToString('N'))"
            $a2 = Find-ByAutomationId -Root $root -AutomationId "A2"
            if ($null -eq $a2) {
                $a2 = Find-ByNameAndControlType -Root $root -Name "A2" `
                    -ControlType ([Windows.Automation.ControlType]::DataItem)
            }
            if ($null -eq $a2) {
                return New-SkipResult `
                    -Reason ("Excel did not expose the A2 grid cell through UI Automation; " +
                        "the edit task requires human handoff on this host.") `
                    -Details @{ processId = $process.Id; workbook = $path }
            }
            $a2Pattern = $null
            if (-not $a2.TryGetCurrentPattern(
                    [Windows.Automation.SelectionItemPattern]::Pattern,
                    [ref]$a2Pattern)) {
                return New-SkipResult `
                    -Reason ("Excel exposed A2 but no SelectionItemPattern, so the suite " +
                        "could not prove which cell would receive keyboard input.") `
                    -Details @{ processId = $process.Id; workbook = $path }
            }
            ([Windows.Automation.SelectionItemPattern]$a2Pattern).Select()
            try {
                $a2.SetFocus()
            }
            catch {
                return New-SkipResult `
                    -Reason ("Excel selected A2 but did not expose a focusable grid cell; " +
                        "no keyboard write was attempted.") `
                    -Details @{
                        processId = $process.Id
                        workbook = $path
                        error = $_.Exception.Message
                    }
            }
            try {
                $null = Wait-Until -TimeoutMilliseconds 3000 -FailureMessage (
                    "Excel did not report A2 as selected before keyboard input."
                ) -Condition {
                    $currentPattern = $null
                    if ($a2.TryGetCurrentPattern(
                            [Windows.Automation.SelectionItemPattern]::Pattern,
                            [ref]$currentPattern)) {
                        return (
                            [Windows.Automation.SelectionItemPattern]$currentPattern
                        ).Current.IsSelected
                    }
                    return $false
                }
                Assert-ForegroundOwnership -Process $process -WindowHandle $handle
                $a2.SetFocus()
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                Write-E2EStage (
                    "excel:focus helper=$($a2.Current.HasKeyboardFocus) " +
                    "pid=$($focused.Current.ProcessId) " +
                    "type=$($focused.Current.ControlType.ProgrammaticName) " +
                    "name='$($focused.Current.Name)' " +
                    "id='$($focused.Current.AutomationId)'"
                )
                [CortexComputerUseE2E.NativeMethods]::SendVirtualKey(0x71)
                Start-Sleep -Milliseconds 500
                $editFocused = [Windows.Automation.AutomationElement]::FocusedElement
                Write-E2EStage (
                    "excel:after-f2 pid=$($editFocused.Current.ProcessId) " +
                    "type=$($editFocused.Current.ControlType.ProgrammaticName) " +
                    "name='$($editFocused.Current.Name)' " +
                    "id='$($editFocused.Current.AutomationId)' " +
                    "hwnd=$($editFocused.Current.NativeWindowHandle)"
                )
                $editWindow = [IntPtr]::new(
                    [long]$editFocused.Current.NativeWindowHandle
                )
                if ($editWindow -eq [IntPtr]::Zero) {
                    throw "Excel edit mode did not expose an exact native keyboard window."
                }
                $editWindowProcessId = [uint32]0
                $null = [CortexComputerUseE2E.NativeMethods]::GetWindowThreadProcessId(
                    $editWindow,
                    [ref]$editWindowProcessId
                )
                if ([int]$editWindowProcessId -ne $process.Id) {
                    throw (
                        "Excel edit window belongs to pid $editWindowProcessId, not " +
                        "the exact launched pid $($process.Id)."
                    )
                }
                $windowEvidence.excelEditWindowId = (
                    Get-WindowHandleHex -WindowHandle $editWindow
                )
                $imeToggleAttempted = $false
                $typedEditText = $null
                try {
                    # The host's active Chinese IME otherwise consumes ASCII keys.
                    # Toggle it for this exact foreground edit control and restore it below.
                    [CortexComputerUseE2E.NativeMethods]::SendVirtualKey(0x10)
                    $imeToggleAttempted = $true
                    Start-Sleep -Milliseconds 250
                    $imeFocused = [Windows.Automation.AutomationElement]::FocusedElement
                    Write-E2EStage (
                        "excel:after-ime-toggle pid=$($imeFocused.Current.ProcessId) " +
                        "type=$($imeFocused.Current.ControlType.ProgrammaticName) " +
                        "name='$($imeFocused.Current.Name)' " +
                        "id='$($imeFocused.Current.AutomationId)' " +
                        "hwnd=$($imeFocused.Current.NativeWindowHandle)"
                    )
                    # F2 moves keyboard focus to a native Excel edit window.
                    # Prove that exact window is foreground instead of trying to
                    # force focus back to the workbook's top-level window, which
                    # would either cancel edit mode or send keys to the grid.
                    Send-KeysToOwnedWindow -Process $process -WindowHandle $editWindow `
                        -Keys $expected
                    Start-Sleep -Milliseconds 500
                    $typedFocused = [Windows.Automation.AutomationElement]::FocusedElement
                    $typedEditText = Get-ControlText -Element $typedFocused
                    Write-E2EStage (
                        "excel:after-text pid=$($typedFocused.Current.ProcessId) " +
                        "type=$($typedFocused.Current.ControlType.ProgrammaticName) " +
                        "name='$($typedFocused.Current.Name)' " +
                        "id='$($typedFocused.Current.AutomationId)' " +
                        "hwnd=$($typedFocused.Current.NativeWindowHandle) " +
                        "text='$typedEditText'"
                    )
                    if ($typedEditText -ne $expected) {
                        throw (
                            "Excel focused edit control did not contain the keyboard " +
                            "input before commit; actual '$typedEditText'."
                        )
                    }
                    [CortexComputerUseE2E.NativeMethods]::SendVirtualKey(0x0D)
                }
                finally {
                    if ($imeToggleAttempted) {
                        Assert-ForegroundOwnership `
                            -Process $process -WindowHandle $handle
                        [CortexComputerUseE2E.NativeMethods]::SendVirtualKey(0x10)
                    }
                }

                $formulaBar = Find-ByNameAndControlType -Root $root `
                    -Name "编辑栏" `
                    -ControlType ([Windows.Automation.ControlType]::Edit)
                if ($null -eq $formulaBar) {
                    $formulaBar = Find-ByNameAndControlType -Root $root `
                        -Name "Formula Bar" `
                        -ControlType ([Windows.Automation.ControlType]::Edit)
                }
                if ($null -eq $formulaBar) {
                    throw "Excel did not expose its formula bar for post-input verification."
                }
                $formulaBarText = Get-ControlText -Element $formulaBar
                $formulaBarMatched = $formulaBarText -eq $expected
                if (-not $formulaBarMatched) {
                    Write-E2EStage (
                        "Excel formula bar did not show the keyboard-entered A2 value; " +
                        "actual '$formulaBarText'."
                    )
                }

                $saveStarted = [DateTime]::UtcNow
                Assert-ForegroundOwnership -Process $process -WindowHandle $handle
                [CortexComputerUseE2E.NativeMethods]::SendControlShortcut(0x53)
            }
            catch {
                throw (
                    "Excel foreground-proven keyboard edit or save failed: " +
                    "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
                )
            }

            $savedValue = Wait-Until -TimeoutMilliseconds 15000 -FailureMessage (
                "Excel did not finish saving the edited workbook."
            ) -PollMilliseconds 250 -Condition {
                $file = Get-Item -LiteralPath $path
                return $file.LastWriteTimeUtc -ge $saveStarted
            }
            $null = Stop-TestProcess -Process $process
            $process = $null
            $actual = Read-ExcelCell -Path $path -Cell "A2"
            if ($actual -ne $expected) {
                throw "Excel file verification failed: expected '$expected', got '$actual'."
            }
            return New-PassResult -Kind "Excel UI edit plus workbook filesystem verifier" `
                -Expected $expected -Actual $actual `
                -Details @{
                    workbook = $path
                    savedValue = $savedValue
                    saveCompletedAt = $saveStarted.ToString("o")
                    isolation = $windowEvidence
                    formulaBarAfterCommit = $formulaBarText
                    formulaBarMatched = $formulaBarMatched
                    editControlBeforeCommit = $typedEditText
                    inputDelivery = (
                        "process-scoped UIA A2 selection, foreground ownership proof, " +
                        "real keyboard text plus Enter, and Ctrl+S; OpenXML was used " +
                        "only to verify the saved file"
                    )
                }
        }

        throw "Unknown Excel acceptance task: $TaskId"
    }
    finally {
        if ($null -ne $process) {
            $null = Stop-TestProcess -Process $process
        }
        Remove-TestDirectory -Path $directory
    }
}

function Invoke-MultiMonitorAction {
    param([string[]]$Allowlist)

    $screens = [System.Windows.Forms.Screen]::AllScreens
    if ($screens.Count -lt 2) {
        return New-SkipResult `
            -Reason "A second monitor is not available; negative-origin and cross-monitor verification was not run." `
            -Details @{ displayCount = $screens.Count }
    }

    $secondary = $screens |
        Where-Object { -not $_.Primary } |
        Sort-Object { $_.Bounds.X }, { $_.Bounds.Y } |
        Select-Object -First 1
    if ($null -eq $secondary) {
        return New-SkipResult -Reason "No non-primary monitor was reported by Windows."
    }

    $process = Start-Fixture `
        -Arguments "--status multi-monitor --exit-timeout-ms 30000"
    try {
        $root = Wait-FixtureElements -Process $process -Allowlist $Allowlist
        $handle = $process.MainWindowHandle
        $x = $secondary.Bounds.X + 80
        $y = $secondary.Bounds.Y + 80
        $width = 640
        $height = 430
        if (-not [CortexComputerUseE2E.NativeMethods]::SetWindowPos(
                $handle,
                [IntPtr]::Zero,
                $x,
                $y,
                $width,
                $height,
                0x0040)) {
            throw "SetWindowPos failed for the exact fixture window."
        }
        $null = Wait-Until -TimeoutMilliseconds 5000 -FailureMessage (
            "Fixture UIA bounds did not move to the secondary monitor."
        ) -Condition {
            $bounds = $root.Current.BoundingRectangle
            return [Math]::Abs([double]$bounds.X - $x) -lt 3 -and
                [Math]::Abs([double]$bounds.Y - $y) -lt 3
        }
        $bounds = $root.Current.BoundingRectangle
        $dpi = [CortexComputerUseE2E.NativeMethods]::GetDpiForWindow($handle)
        return New-PassResult -Kind "secondary monitor placement and DPI evidence" `
            -Expected @{
                display = $secondary.DeviceName
                x = $x
                y = $y
            } -Actual @{
                display = $secondary.DeviceName
                bounds = @{
                    x = [double]$bounds.X
                    y = [double]$bounds.Y
                    width = [double]$bounds.Width
                    height = [double]$bounds.Height
                }
                dpi = $dpi
                scale = [Math]::Round($dpi / 96.0, 3)
            } -Details @{ processId = $process.Id }
    }
    finally {
        $null = Stop-TestProcess -Process $process
    }
}

function Invoke-AcceptanceTask {
    param(
        [string]$TaskId,
        [string[]]$Allowlist
    )

    if ($TaskId -like "fixture-*") {
        return Invoke-FixtureAction -TaskId $TaskId -Allowlist $Allowlist
    }
    if ($TaskId -like "chrome-*") {
        return Invoke-ChromeAction -TaskId $TaskId -Allowlist $Allowlist
    }
    if ($TaskId -like "notepad-*") {
        return Invoke-NotepadAction -TaskId $TaskId -Allowlist $Allowlist
    }
    if ($TaskId -like "explorer-*") {
        return Invoke-ExplorerAction -TaskId $TaskId -Allowlist $Allowlist
    }
    if ($TaskId -like "excel-*") {
        return Invoke-ExcelAction -TaskId $TaskId -Allowlist $Allowlist
    }
    if ($TaskId -eq "multi-monitor-negative-origin") {
        return Invoke-MultiMonitorAction -Allowlist $Allowlist
    }
    throw "Unknown acceptance task: $TaskId"
}

function Get-TaskById {
    param([string]$TaskId)

    $definition = $script:Tasks | Where-Object { $_.Id -eq $TaskId } |
        Select-Object -First 1
    if ($null -eq $definition) {
        throw "Unknown task id: $TaskId"
    }
    return $definition
}

function Write-JsonFile {
    param(
        [object]$Value,
        [string]$Path
    )

    $directory = Split-Path -Parent $Path
    if (-not [string]::IsNullOrWhiteSpace($directory)) {
        $null = New-Item -ItemType Directory -Path $directory -Force
    }
    $json = $Value | ConvertTo-Json -Depth 16
    [System.IO.File]::WriteAllText(
        $Path,
        $json + [Environment]::NewLine,
        [System.Text.UTF8Encoding]::new($false)
    )
    if ($env:CORTEX_E2E_DEBUG -eq "1") {
        $debugPath = [System.IO.Path]::ChangeExtension($Path, ".debug.json")
        [System.IO.File]::WriteAllText(
            $debugPath,
            $json + [Environment]::NewLine,
            [System.Text.UTF8Encoding]::new($false)
        )
    }
}

function Invoke-WorkerMode {
    param(
        [string]$TaskId,
        [string]$Output,
        [string]$Gate
    )

    if ([string]::IsNullOrWhiteSpace($Output)) {
        throw "-WorkerOutput is required in worker mode."
    }
    if ([string]::IsNullOrWhiteSpace($Gate)) {
        throw "-WorkerGate is required in worker mode."
    }

    $gateDeadline = [DateTime]::UtcNow.AddSeconds(15)
    while (-not (Test-Path -LiteralPath $Gate)) {
        if ([DateTime]::UtcNow -ge $gateDeadline) {
            throw "Worker start gate was not released."
        }
        Start-Sleep -Milliseconds 25
    }

    # worker 在生成 fixture/浏览器子进程之前先把自己加入
    # kill-on-close Job Object。父测试进程只需要等待和超时终止 worker，
    # 不再在 worker 已经启动后再竞争式地改它的 job membership。
    $workerJob = New-KillOnCloseJob
    $currentProcess = [System.Diagnostics.Process]::GetCurrentProcess()
    if (-not [CortexComputerUseE2E.NativeMethods]::AssignProcessToJobObject(
            $workerJob,
            $currentProcess.Handle)) {
        throw "Unable to place the task worker in its kill-on-close Job Object."
    }

    $definition = Get-TaskById -TaskId $TaskId
    $startedAt = [DateTimeOffset]::UtcNow
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $body = Invoke-AcceptanceTask -TaskId $TaskId -Allowlist $definition.Allowlist
    }
    catch {
        $body = [ordered]@{
            status = "fail"
            reason = $_.Exception.Message
            verifier = $null
            details = [ordered]@{
                exceptionType = $_.Exception.GetType().FullName
                scriptStackTrace = $_.ScriptStackTrace
                failurePosition = $_.InvocationInfo.PositionMessage
            }
            cleanup = @("worker exception captured")
        }
    }
    finally {
        $stopwatch.Stop()
    }
    if ($env:CORTEX_E2E_DEBUG -eq "1") {
        $bodyItems = @($body)
        Write-E2EStage ("task-result-count:" + $bodyItems.Count)
        foreach ($bodyItem in $bodyItems) {
            if ($null -eq $bodyItem) {
                Write-E2EStage "task-result-item:<null>"
            }
            else {
                Write-E2EStage (
                    "task-result-item:" + $bodyItem.GetType().FullName +
                    ":" + ($bodyItem | Out-String -Width 240).Trim()
                )
            }
        }
    }

    $result = [ordered]@{
        id = $definition.Id
        family = $definition.Family
        application = $definition.Application
        status = $body.status
        reason = $body.reason
        timeoutSeconds = $definition.TimeoutSeconds
        allowlist = @($definition.Allowlist)
        verifier = $body.verifier
        details = $body.details
        cleanup = @($body.cleanup)
        startedAt = $startedAt.ToString("o")
        durationMs = $stopwatch.ElapsedMilliseconds
    }
    Write-JsonFile -Value $result -Path $Output
}

function New-KillOnCloseJob {
    $job = [CortexComputerUseE2E.NativeMethods]::CreateJobObject(
        [IntPtr]::Zero,
        $null
    )
    if ($job -eq [IntPtr]::Zero) {
        throw "CreateJobObject failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }

    $information = New-Object CortexComputerUseE2E.JOBOBJECT_EXTENDED_LIMIT_INFORMATION
    $information.BasicLimitInformation.LimitFlags =
        [CortexComputerUseE2E.NativeMethods]::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    $size = [Runtime.InteropServices.Marshal]::SizeOf(
        [type][CortexComputerUseE2E.JOBOBJECT_EXTENDED_LIMIT_INFORMATION]
    )
    $pointer = [Runtime.InteropServices.Marshal]::AllocHGlobal($size)
    try {
        [Runtime.InteropServices.Marshal]::StructureToPtr($information, $pointer, $false)
        $ok = [CortexComputerUseE2E.NativeMethods]::SetInformationJobObject(
            $job,
            [CortexComputerUseE2E.NativeMethods]::JobObjectExtendedLimitInformation,
            $pointer,
            [uint32]$size
        )
        if (-not $ok) {
            throw "SetInformationJobObject failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
        }
    }
    catch {
        $null = [CortexComputerUseE2E.NativeMethods]::CloseHandle($job)
        throw
    }
    finally {
        [Runtime.InteropServices.Marshal]::FreeHGlobal($pointer)
    }
    return $job
}

function Invoke-TaskInWorker {
    param([pscustomobject]$Definition)

    $workerDirectory = New-TestDirectory -Purpose ("worker-" + $Definition.Id)
    $workerOutput = Join-Path $workerDirectory "result.json"
    $gatePath = Join-Path $workerDirectory "start.gate"
    $powerShellExe = Join-Path $env:WINDIR "System32\WindowsPowerShell\v1.0\powershell.exe"
    $arguments = @(
        "-NoProfile"
        "-NonInteractive"
        "-STA"
        "-ExecutionPolicy"
        "Bypass"
        "-File"
        (Quote-ProcessArgument $script:ScriptPath)
        "-InternalTask"
        (Quote-ProcessArgument $Definition.Id)
        "-WorkerOutput"
        (Quote-ProcessArgument $workerOutput)
        "-WorkerGate"
        (Quote-ProcessArgument $gatePath)
        "-NoBuild"
        "-Task"
        (Quote-ProcessArgument $Definition.Id)
    ) -join " "

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $powerShellExe
    $startInfo.Arguments = $arguments
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $worker = [System.Diagnostics.Process]::new()
    $worker.StartInfo = $startInfo
    $timedOut = $false
    try {
        if (-not $worker.Start()) {
            throw "Failed to start PowerShell worker for $($Definition.Id)."
        }
        [System.IO.File]::WriteAllText($gatePath, "start")

        if (-not $worker.WaitForExit($Definition.TimeoutSeconds * 1000)) {
            $timedOut = $true
            try {
                $worker.Kill()
            }
            catch {
            }
            $null = $worker.WaitForExit(5000)
        }

        if ($timedOut) {
            return [ordered]@{
                id = $Definition.Id
                family = $Definition.Family
                application = $Definition.Application
                status = "fail"
                reason = "Task exceeded its hard timeout of $($Definition.TimeoutSeconds) seconds."
                timeoutSeconds = $Definition.TimeoutSeconds
                allowlist = @($Definition.Allowlist)
                verifier = $null
                details = [ordered]@{
                    workerOutput = "worker console output is streamed to the suite console"
                }
                cleanup = @("terminating the worker closes its kill-on-close Job Object and its child process tree")
                startedAt = $null
                durationMs = $Definition.TimeoutSeconds * 1000
            }
        }

        if (-not (Test-Path -LiteralPath $workerOutput -PathType Leaf)) {
            return [ordered]@{
                id = $Definition.Id
                family = $Definition.Family
                application = $Definition.Application
                status = "fail"
                reason = "Worker exited without producing a machine-readable result."
                timeoutSeconds = $Definition.TimeoutSeconds
                allowlist = @($Definition.Allowlist)
                verifier = $null
                details = [ordered]@{ exitCode = $worker.ExitCode }
        cleanup = @("worker process exited; its kill-on-close Job Object released")
                startedAt = $null
                durationMs = $null
            }
        }

        $workerJson = [System.IO.File]::ReadAllText($workerOutput)
        return ($workerJson | ConvertFrom-Json)
    }
    finally {
        if (-not $worker.HasExited) {
            try { $worker.Kill() } catch {}
        }
        $worker.Dispose()
        try {
            Remove-TestDirectory -Path $workerDirectory
        }
        catch {
            Write-Warning (
                "Could not remove worker artifact directory '$workerDirectory': " +
                $_.Exception.Message
            )
        }
    }
}

function Invoke-Suite {
    param(
        [string[]]$RequestedTasks,
        [string]$Output
    )

    if ([string]::IsNullOrWhiteSpace($Output)) {
        $Output = Join-Path $script:ResultRoot "latest.json"
    }
    $Output = [System.IO.Path]::GetFullPath($Output)

    if (-not $NoBuild) {
        & (Join-Path $script:FixtureRoot "build.ps1")
        if ($LASTEXITCODE -ne 0) {
            throw "Fixture build failed with exit code $LASTEXITCODE."
        }
    }
    if (-not (Test-Path -LiteralPath $script:FixtureExe -PathType Leaf)) {
        throw "Fixture executable is missing: $script:FixtureExe"
    }

    $selected = @(
        if ($null -eq $RequestedTasks -or $RequestedTasks.Count -eq 0) {
            @($script:Tasks)
        }
        else {
            @(
                foreach ($requested in $RequestedTasks) {
                    foreach ($taskId in $requested.Split(",")) {
                        $trimmed = $taskId.Trim()
                        if (-not [string]::IsNullOrWhiteSpace($trimmed)) {
                            Get-TaskById -TaskId $trimmed
                        }
                    }
                }
            )
        }
    )
    if ($selected.Count -eq 0) {
        throw "No tasks selected."
    }

    $startedAt = [DateTimeOffset]::UtcNow
    $suiteStopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $results = [System.Collections.Generic.List[object]]::new()
    foreach ($definition in $selected) {
        Write-Host "RUN $($definition.Family)/$($definition.Id) (timeout $($definition.TimeoutSeconds)s)"
        $result = Invoke-TaskInWorker -Definition $definition
        $results.Add($result)
        Write-Host ("{0} {1} ({2} ms)" -f $result.status.ToUpperInvariant(), $result.id, $result.durationMs)
    }
    $suiteStopwatch.Stop()

    $pass = @($results | Where-Object { $_.status -eq "pass" }).Count
    $fail = @($results | Where-Object { $_.status -eq "fail" }).Count
    $skipped = @($results | Where-Object { $_.status -eq "skipped" }).Count
    $screens = [System.Windows.Forms.Screen]::AllScreens
    $report = [ordered]@{
        schemaVersion = 1
        suite = "cortex-computer-use-windows-e2e"
        scope = "harness_only"
        scopeDescription = "PowerShell + UIA/CDP application harness; this report does not represent the Cortex model, confirmation, Gateway or trace path"
        generatedAt = $startedAt.ToString("o")
        durationMs = $suiteStopwatch.ElapsedMilliseconds
        host = [ordered]@{
            computerName = $env:COMPUTERNAME
            os = [Environment]::OSVersion.VersionString
            sessionId = (Get-Process -Id $PID).SessionId
            desktopLockedAtReport = (Test-WindowsSessionLocked)
            displayCount = $screens.Count
            displays = @(
                $screens | ForEach-Object {
                    [ordered]@{
                        deviceName = $_.DeviceName
                        primary = $_.Primary
                        bounds = @{
                            x = $_.Bounds.X
                            y = $_.Bounds.Y
                            width = $_.Bounds.Width
                            height = $_.Bounds.Height
                        }
                    }
                }
            )
        }
        execution = [ordered]@{
            isolation = "one Windows Job Object per task (JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)"
            ui = "PowerShell + Windows UI Automation"
            appAllowlist = "exact executable paths plus process-scoped UIA roots"
            resultPolicy = "only executed checks may pass; unavailable dependencies are skipped with a reason"
        }
        summary = [ordered]@{
            total = $results.Count
            pass = $pass
            fail = $fail
            skipped = $skipped
        }
        tasks = @($results)
    }
    Write-JsonFile -Value $report -Path $Output
    Write-Output ($report | ConvertTo-Json -Depth 16)
}

if (-not [string]::IsNullOrWhiteSpace($WorkerOutput) -and
    -not [string]::IsNullOrWhiteSpace($WorkerGate)) {
    Invoke-WorkerMode -TaskId $InternalTask -Output $WorkerOutput -Gate $WorkerGate
}
else {
    Invoke-Suite -RequestedTasks $Task -Output $OutputPath
}
