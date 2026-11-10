#requires -Version 5.1
[CmdletBinding(DefaultParameterSetName = 'Benchmark')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Benchmark')][string]$OfficialDriverPath,
    [Parameter(ParameterSetName = 'Benchmark')]
    [string]$OfficialExpectedSha256 = 'ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2',
    [Parameter(Mandatory = $true, ParameterSetName = 'MatchedControl')][string]$ControlDriverPath,
    [Parameter(Mandatory = $true, ParameterSetName = 'MatchedControl')][string]$ControlExpectedSha256,
    [Parameter(Mandatory = $true, ParameterSetName = 'MatchedControl')][string]$ControlBuildProofPath,
    [Parameter(Mandatory = $true, ParameterSetName = 'Benchmark')]
    [Parameter(Mandatory = $true, ParameterSetName = 'MatchedControl')][string]$CandidateDriverPath,
    [Parameter(Mandatory = $true, ParameterSetName = 'Benchmark')]
    [Parameter(Mandatory = $true, ParameterSetName = 'MatchedControl')][string]$CandidateExpectedSha256,
    [Parameter(Mandatory = $true, ParameterSetName = 'Benchmark')]
    [Parameter(Mandatory = $true, ParameterSetName = 'MatchedControl')][string]$FixturePath,
    [Parameter(Mandatory = $true, ParameterSetName = 'Benchmark')]
    [Parameter(Mandatory = $true, ParameterSetName = 'MatchedControl')][string]$FixtureExpectedSha256,
    [Parameter(Mandatory = $true, ParameterSetName = 'Benchmark')]
    [Parameter(Mandatory = $true, ParameterSetName = 'MatchedControl')][string]$OutputPath,
    [ValidateRange(1, 1000)][int]$Warmup = 25,
    [ValidateRange(1, 20000)][int]$Observations = 1000,
    [ValidateRange(1, 1000)][int]$SampleEvery = 25,
    [ValidateRange(0, 10000)][int]$IntervalMs = 100,
    [ValidateRange(1000, 30000)][int]$CallTimeoutMs = 20000,
    [ValidateRange(30, 1800)][int]$GroupTimeoutSeconds = 600,
    [ValidateRange(1, 30)][int]$PostEndSamples = 3,
    [ValidateRange(10, 10000)][int]$PostEndIntervalMs = 1000,
    [Parameter(Mandatory = $true, ParameterSetName = 'SelfTest')][switch]$SelfTest,
    [Parameter(Mandatory = $true, ParameterSetName = 'SourceOnly')][switch]$SourceOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:Scope = 'driver_readonly_fixture_ab'
$script:PinnedOfficialSha = 'ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2'
$script:AllowedTools = @('start_session', 'get_window_state', 'end_session')
$script:FixtureTitle = 'Cortex Computer Use Fixture'

function Require-AB {
    param([bool]$Condition, [string]$Code)
    if (-not $Condition) { throw [InvalidOperationException]::new($Code) }
}

function Get-ABField {
    param($Value, [string]$Name)
    if ($null -eq $Value) { return $null }
    if ($Value -is [Collections.IDictionary]) {
        if ($Value.Contains($Name)) { return ,($Value[$Name]) }
        return $null
    }
    $property = $Value.PSObject.Properties[$Name]
    if ($null -ne $property) { return ,($property.Value) }
    return $null
}

function Test-ABInteger {
    param($Value)
    return ($Value -is [int] -or $Value -is [long] -or $Value -is [uint32] -or $Value -is [uint64])
}

function Assert-ABExitCode {
    param($ExitCode)
    Require-AB ((Test-ABInteger $ExitCode) -and $ExitCode -eq 0) 'AB_CLI_EXIT_CODE'
}

function Assert-ABSession {
    param($Payload, [string]$Session, [bool]$Active)
    $actual = Get-ABField $Payload 'active'
    Require-AB ($actual -is [bool] -and $actual -eq $Active) 'AB_SESSION_ACTIVE'
    Require-AB ((Get-ABField $Payload 'session') -ceq $Session) 'AB_SESSION_IDENTITY'
}

function Assert-ABControlBuildProof {
    param($Proof, [string]$ExpectedSha)
    Require-AB ((Get-ABField $Proof 'evidence_kind') -ceq 'control_build_only' -and
        (Get-ABField $Proof 'base_commit') -ceq '85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19' -and
        (Get-ABField $Proof 'toolchain') -ceq '1.98.0-x86_64-pc-windows-msvc' -and
        (Get-ABField $Proof 'exit_code') -eq 0 -and
        (Get-ABField $Proof 'binary_sha256') -ceq $ExpectedSha.ToLowerInvariant() -and
        (Get-ABField $Proof 'manifest_and_lock_unchanged') -ceq $true -and
        (Get-ABField $Proof 'control_source_unchanged') -ceq $true -and
        (Get-ABField $Proof 'candidate_source_and_binary_unchanged') -ceq $true) 'AB_CONTROL_BUILD_PROOF'
}

function Assert-ABStatus {
    param([string]$Text)
    Assert-ABNoImages $Text
    Require-AB ($Text -cmatch 'permission mode:\s*bounded\b' -and
        $Text -cmatch '(?:^|[^A-Za-z0-9_])approved_at_startup=true(?:$|[^A-Za-z0-9_])' -and
        $Text -cmatch '(?:^|[^A-Za-z0-9_])valid=true(?:$|[^A-Za-z0-9_])') 'AB_DAEMON_NOT_READY'
}

function Assert-ABNoImages {
    param($Value)
    if ($null -eq $Value) { return }
    if ($Value -is [string]) {
        Require-AB (-not ($Value -match '(?i)data:image/' -or
            $Value.Contains('iVBORw0KGgo') -or $Value.Contains('/9j/'))) 'AB_IMAGE_TEXT'
        return
    }
    if ($Value -is [Collections.IDictionary]) {
        $entries = @($Value.GetEnumerator() | ForEach-Object {
            [pscustomobject]@{ Name = [string]$_.Key; Value = $_.Value }
        })
    }
    elseif ($Value -is [Array]) {
        foreach ($item in $Value) { Assert-ABNoImages $item }
        return
    }
    elseif ($Value -is [pscustomobject]) { $entries = @($Value.PSObject.Properties) }
    else { return }
    foreach ($entry in $entries) {
        $name = $entry.Name.ToLowerInvariant()
        $item = $entry.Value
        if ($name -eq 'type') {
            Require-AB ([string]$item -notmatch '^(?i:image|image_url)$') 'AB_IMAGE_CONTENT'
        }
        if ($name -eq 'mimetype' -or $name -eq 'mime_type') {
            Require-AB ([string]$item -notmatch '^(?i:image/)') 'AB_IMAGE_RESOURCE'
        }
        if ($name -in @('screenshot_png_b64', 'screenshot_base64', 'image_base64',
                'capture_png_b64', 'screenshot_path', 'screenshot_file_path', 'screenshot_out_file', 'blob',
                'capture_id', 'screenshot_width', 'screenshot_height')) {
            $empty = ($null -eq $item -or ($item -is [string] -and $item -ceq '') -or
                ((Test-ABInteger $item) -and $item -eq 0) -or ($item -is [bool] -and -not $item))
            Require-AB $empty 'AB_IMAGE_PAYLOAD'
        }
        Assert-ABNoImages $item
    }
}

function Assert-ABState {
    param($State, [int]$FixturePid, [long]$WindowId,
        [Collections.Generic.HashSet[string]]$SeenSnapshots)
    Assert-ABNoImages $State
    Require-AB ((Test-ABInteger (Get-ABField $State 'pid')) -and
        (Get-ABField $State 'pid') -eq $FixturePid) 'AB_STATE_PID'
    Require-AB ((Test-ABInteger (Get-ABField $State 'window_id')) -and
        (Get-ABField $State 'window_id') -eq $WindowId) 'AB_STATE_HWND'
    Require-AB ((Get-ABField $State 'window_title') -ceq $script:FixtureTitle) 'AB_STATE_TITLE'
    $snapshot = Get-ABField $State 'snapshot_id'
    Require-AB ($snapshot -is [string] -and -not [string]::IsNullOrWhiteSpace($snapshot)) 'AB_SNAPSHOT_MISSING'
    Require-AB (-not $SeenSnapshots.Contains($snapshot)) 'AB_SNAPSHOT_REUSED'
    Require-AB ((Get-ABField $State 'tree_format') -ceq 'both') 'AB_TREE_FORMAT'
    $tree = Get-ABField $State 'tree_markdown'
    Require-AB ($tree -is [string] -and -not [string]::IsNullOrWhiteSpace($tree)) 'AB_TREE_MISSING'
    $complete = Get-ABField $State 'elements_complete'
    $truncated = Get-ABField $State 'truncated'
    Require-AB ($complete -is [bool] -and $complete -and
        $truncated -is [bool] -and -not $truncated) 'AB_TREE_INCOMPLETE'
    foreach ($name in @('degraded', 'is_delta', 'unchanged', 'isError', 'is_error')) {
        $flag = Get-ABField $State $name
        Require-AB ($null -eq $flag -or ($flag -is [bool] -and -not $flag)) 'AB_STATE_DEGRADED'
    }
    $validFrame = Get-ABField $State 'screenshot_frame_valid'
    Require-AB ($null -eq $validFrame -or ($validFrame -is [bool] -and $validFrame)) 'AB_STATE_FRAME_INVALID'
    Require-AB ($null -eq (Get-ABField $State 'error')) 'AB_STATE_ERROR'
    $elements = Get-ABField $State 'elements'
    Require-AB ($elements -is [Array] -and $elements.Count -gt 0) 'AB_ELEMENTS_EMPTY'
    $total = Get-ABField $State 'total_element_count'
    $returned = Get-ABField $State 'returned_element_count'
    Require-AB ((Test-ABInteger $total) -and (Test-ABInteger $returned) -and
        $total -eq $returned -and $returned -eq $elements.Count) 'AB_ELEMENT_COUNT'
    foreach ($element in $elements) {
        $token = Get-ABField $element 'element_token'
        Require-AB ($token -is [string] -and $token.StartsWith($snapshot + ':', [StringComparison]::Ordinal)) 'AB_ELEMENT_TOKEN'
    }
    $null = $SeenSnapshots.Add($snapshot)
    # 只返回数字；AX 正文、ref、snapshot id 留在当前调用内存，不进入报告。
    return [pscustomobject]@{ elements = $elements.Count; tree_chars = $tree.Length }
}

function New-ABStateArguments {
    param([int]$FixturePid, [long]$WindowId, [string]$Session)
    return [ordered]@{
        pid = $FixturePid; window_id = $WindowId; session = $Session
        tree_format = 'both'; include_accessibility_tree = $true
        include_screenshot = $false; since = $null
        max_elements = 250; timeout_ms = 10000
    }
}

function New-ABManifest {
    param([string]$FixtureExecutable)
    return [ordered]@{
        version = 3; expires_after = '35m'; idle_timeout = '1m'
        resources = [ordered]@{
            apps = @([ordered]@{ executable = $FixtureExecutable; windows = 'all'; terminate = 'deny' })
        }
        allow = [ordered]@{ tools = @($script:AllowedTools) }
    }
}

function Assert-ABManifest {
    param($Manifest, [string]$FixtureExecutable)
    $resources = Get-ABField $Manifest 'resources'
    $apps = Get-ABField $resources 'apps'
    $tools = Get-ABField (Get-ABField $Manifest 'allow') 'tools'
    Require-AB ((Get-ABField $Manifest 'version') -eq 3 -and
        (Get-ABField $Manifest 'expires_after') -ceq '35m' -and
        (Get-ABField $Manifest 'idle_timeout') -ceq '1m') 'AB_MANIFEST_LIFETIME'
    Require-AB ($resources.Count -eq 1 -and $apps -is [Array] -and $apps.Count -eq 1 -and
        (Get-ABField $apps[0] 'executable') -ceq $FixtureExecutable -and
        (Get-ABField $apps[0] 'windows') -ceq 'all' -and
        (Get-ABField $apps[0] 'terminate') -ceq 'deny') 'AB_MANIFEST_RESOURCE'
    Require-AB ($tools -is [Array] -and $tools.Count -eq 3 -and
        (@($tools | Sort-Object) -join ',') -ceq (@($script:AllowedTools | Sort-Object) -join ',')) 'AB_MANIFEST_TOOLS'
}

function ConvertTo-ABWindowsArgument {
    param([AllowEmptyString()][string]$Value)
    # Windows CRT 的引号/末尾反斜杠规则；不通过 shell 拼接或执行命令。
    $builder = [Text.StringBuilder]::new()
    $null = $builder.Append('"')
    $slashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq '\') { $slashes++; continue }
        if ($character -eq '"') {
            $null = $builder.Append(('\' * ($slashes * 2 + 1)))
        }
        else { $null = $builder.Append(('\' * $slashes)) }
        $null = $builder.Append($character)
        $slashes = 0
    }
    $null = $builder.Append(('\' * ($slashes * 2)))
    $null = $builder.Append('"')
    return $builder.ToString()
}

function Initialize-ABNative {
    if ('CuaReadonlyAB.Pump' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
namespace CuaReadonlyAB {
    public sealed class Pump {
        private readonly StreamReader reader;
        private readonly int limit;
        private readonly StringBuilder text = new StringBuilder();
        private long count;
        private int overflow;
        public Task Completion { get; private set; }
        public long Count { get { return Interlocked.Read(ref count); } }
        public bool Overflow { get { return Volatile.Read(ref overflow) != 0; } }
        public string Text { get { lock (text) { return text.ToString(); } } }
        public Pump(StreamReader reader, int limit) {
            this.reader = reader; this.limit = limit;
            Completion = Task.Run(async () => {
                char[] buffer = new char[4096];
                int n;
                while ((n = await reader.ReadAsync(buffer, 0, buffer.Length).ConfigureAwait(false)) > 0) {
                    Interlocked.Add(ref count, n);
                    if (limit > 0) {
                        lock (text) {
                            int take = Math.Min(n, Math.Max(0, limit - text.Length));
                            text.Append(buffer, 0, take);
                            if (take != n) Interlocked.Exchange(ref overflow, 1);
                        }
                    }
                }
            });
        }
    }
    // 仅持有有界数字与固定步骤名。后台每 2 秒写一次，不让步骤标记本身阻塞 Check。
    public sealed class StepJournal : IDisposable {
        readonly object gate = new object();
        readonly string path;
        readonly Stopwatch clock;
        readonly Timer timer;
        string stage = "Idle", step = "Idle";
        long started, sequence, observations;
        int session, busy, closed;
        long writes, errors, lastIoMs;
        public long Writes { get { return Interlocked.Read(ref writes); } }
        public long Errors { get { return Interlocked.Read(ref errors); } }
        public long LastIoMs { get { return Interlocked.Read(ref lastIoMs); } }
        public bool Drained { get { return Volatile.Read(ref busy) == 0; } }
        public StepJournal(string path, Stopwatch clock) {
            this.path = path; this.clock = clock;
            if (File.Exists(path) || File.Exists(path + ".next")) throw new IOException("AB_STEP_OUTPUT_EXISTS");
            timer = new Timer(WriteSnapshot, null, 0, 2000);
        }
        static bool SafeName(string value) {
            if (String.IsNullOrEmpty(value) || value.Length > 96) return false;
            foreach (char c in value) if (!(c >= 'a' && c <= 'z') && !(c >= 'A' && c <= 'Z') && c != '.' && c != '_') return false;
            return true;
        }
        public void Update(string category, string name, long start, long seq, long obs, int ordinal) {
            if (!SafeName(category) || !SafeName(name)) throw new InvalidOperationException("AB_STEP_NAME");
            lock (gate) { stage = category; step = name; started = start; sequence = seq; observations = obs; session = ordinal; }
        }
        public string Snapshot() {
            lock (gate) {
                return String.Format(System.Globalization.CultureInfo.InvariantCulture,
                    "{{\"schema_version\":1,\"stage\":\"{0}\",\"step\":\"{1}\",\"step_started_elapsed_ms\":{2},\"heartbeat_elapsed_ms\":{3},\"sequence\":{4},\"observations_completed\":{5},\"session_ordinal\":{6},\"previous_write_ms\":{7},\"write_errors\":{8}}}",
                    stage, step, started, clock.ElapsedMilliseconds, sequence, observations, session, LastIoMs, Errors);
            }
        }
        void WriteSnapshot(object unused) {
            if (Volatile.Read(ref closed) != 0 || Interlocked.CompareExchange(ref busy, 1, 0) != 0) return;
            if (Volatile.Read(ref closed) != 0) { Volatile.Write(ref busy, 0); return; }
            var watch = Stopwatch.StartNew();
            string temporary = path + ".next";
            try {
                // 不 fsync；只有一个后台写入者，避免诊断日志反过来制造同步磁盘等待。
                File.WriteAllText(temporary, Snapshot(), new UTF8Encoding(false));
                if (File.Exists(path)) File.Replace(temporary, path, null); else File.Move(temporary, path);
                Interlocked.Increment(ref writes);
            } catch { Interlocked.Increment(ref errors); }
            finally {
                try { if (File.Exists(temporary)) File.Delete(temporary); } catch { Interlocked.Increment(ref errors); }
                Interlocked.Exchange(ref lastIoMs, watch.ElapsedMilliseconds);
                Volatile.Write(ref busy, 0);
            }
        }
        public void Dispose() {
            Interlocked.Exchange(ref closed, 1); timer.Dispose();
            var watch = Stopwatch.StartNew();
            while (!Drained && watch.ElapsedMilliseconds < 2000) Thread.Sleep(10);
        }
    }
    public static class Windows {
        [StructLayout(LayoutKind.Sequential)] private struct FileTime { public uint Low, High; }
        [DllImport("kernel32.dll", SetLastError=true)] private static extern bool GetProcessTimes(IntPtr process, out FileTime creation, out FileTime exit, out FileTime kernel, out FileTime user);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true, ExactSpelling=true)]
        private static extern bool QueryFullProcessImageNameW(IntPtr process, uint flags, StringBuilder image, ref uint size);
        public static long CreationTicks(Process process) {
            FileTime creation, exit, kernel, user;
            if (!GetProcessTimes(process.Handle, out creation, out exit, out kernel, out user))
                throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
            return DateTime.FromFileTimeUtc(((long)creation.High << 32) | creation.Low).Ticks;
        }
        public static string ImagePath(Process process) {
            // 读原进程句柄的内核映像路径，不依赖启动期间的 PEB/模块枚举。
            uint size = 32768; var image = new StringBuilder((int)size);
            if (!QueryFullProcessImageNameW(process.Handle, 0, image, ref size))
                throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
            return image.ToString();
        }
        // 与 crates/cortex-local/src/computer_desktop.rs 的 Win10/11 证据链一致。
        // WTSINFOEX_LEVEL1_W 的完整布局用于核对缓冲区大小，不能只信前几个字段。
        [StructLayout(LayoutKind.Sequential)] private struct SessionInfoLevel1 {
            public uint SessionId;
            public int SessionState, SessionFlags;
            [MarshalAs(UnmanagedType.ByValArray, SizeConst=33)] public ushort[] WinStationName;
            [MarshalAs(UnmanagedType.ByValArray, SizeConst=21)] public ushort[] UserName;
            [MarshalAs(UnmanagedType.ByValArray, SizeConst=18)] public ushort[] DomainName;
            public long LogonTime, ConnectTime, DisconnectTime, LastInputTime, CurrentTime;
            public uint IncomingBytes, OutgoingBytes, IncomingFrames, OutgoingFrames;
            public uint IncomingCompressedBytes, OutgoingCompressedBytes;
        }
        [StructLayout(LayoutKind.Sequential)] private struct SessionInfoEx {
            public uint Level;
            public SessionInfoLevel1 Data;
        }
        private delegate bool EnumProc(IntPtr hwnd, IntPtr param);
        [DllImport("user32.dll")] private static extern bool EnumThreadWindows(uint tid, EnumProc cb, IntPtr p);
        [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
        [DllImport("user32.dll")] private static extern IntPtr GetWindow(IntPtr hwnd, uint cmd);
        [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
        [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
        [DllImport("kernel32.dll")] private static extern uint GetCurrentProcessId();
        [DllImport("kernel32.dll")] private static extern bool ProcessIdToSessionId(uint pid, out uint session);
        [DllImport("wtsapi32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
        private static extern bool WTSQuerySessionInformationW(IntPtr server, uint session, int infoClass, out IntPtr buffer, out uint bytes);
        [DllImport("wtsapi32.dll")] private static extern void WTSFreeMemory(IntPtr buffer);
        [DllImport("user32.dll")] private static extern IntPtr GetProcessWindowStation();
        [DllImport("user32.dll")] private static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
        [DllImport("user32.dll")] private static extern bool CloseDesktop(IntPtr desktop);
        [DllImport("user32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
        private static extern bool GetUserObjectInformationW(IntPtr handle, int index, IntPtr buffer, uint length, out uint needed);
        [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetWindowText(IntPtr hwnd, StringBuilder s, int max);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
        private static extern bool WaitNamedPipe(string name, uint timeout);
        public static bool PipeReady(string name) { return WaitNamedPipe(name, 100); }
        public static bool SessionIdentityAllowed(uint processSession, uint level, uint reportedSession) {
            return processSession != 0 && level == 1 && reportedSession == processSession;
        }
        public static bool DesktopEvidenceAllowed(int state, uint flags, string station, string desktop,
                bool hasForeground, string foregroundProcess) {
            // WTSActive=0，WTS_SESSIONSTATE_UNLOCK=1；空前台窗口不是锁屏证据。
            if (state != 0 || flags != 1
                    || !String.Equals(station, "WinSta0", StringComparison.OrdinalIgnoreCase)
                    || !String.Equals(desktop, "Default", StringComparison.OrdinalIgnoreCase)) return false;
            if (!hasForeground) return true;
            return !String.IsNullOrWhiteSpace(foregroundProcess)
                && !String.Equals(foregroundProcess, "LockApp", StringComparison.OrdinalIgnoreCase)
                && !String.Equals(foregroundProcess, "LogonUI", StringComparison.OrdinalIgnoreCase)
                && !String.Equals(foregroundProcess, "Winlogon", StringComparison.OrdinalIgnoreCase);
        }
        private static string ObjectName(IntPtr handle) {
            if (handle == IntPtr.Zero) return null;
            uint needed;
            GetUserObjectInformationW(handle, 2, IntPtr.Zero, 0, out needed); // UOI_NAME=2
            if (needed < 2 || needed > 1024 || (needed & 1) != 0) return null;
            uint capacity = needed;
            IntPtr buffer = Marshal.AllocHGlobal((int)capacity);
            try {
                if (!GetUserObjectInformationW(handle, 2, buffer, capacity, out needed)
                        || needed < 2 || needed > capacity || (needed & 1) != 0) return null;
                var bytes = new byte[needed];
                Marshal.Copy(buffer, bytes, 0, bytes.Length);
                int end = 0;
                while (end < bytes.Length && (bytes[end] != 0 || bytes[end + 1] != 0)) end += 2;
                // 和 Rust String::from_utf16 一样，拒绝无效 UTF-16，不能静默替换。
                return new UnicodeEncoding(false, false, true).GetString(bytes, 0, end);
            } finally { Marshal.FreeHGlobal(buffer); }
        }
        public static bool InteractiveDesktop() {
            IntPtr sessionBuffer = IntPtr.Zero, inputDesktop = IntPtr.Zero;
            try {
                uint session, bytes;
                if (!ProcessIdToSessionId(GetCurrentProcessId(), out session) || session == 0) return false;
                if (!WTSQuerySessionInformationW(IntPtr.Zero, session, 25, out sessionBuffer, out bytes)
                        || sessionBuffer == IntPtr.Zero || bytes < Marshal.SizeOf(typeof(SessionInfoEx))) return false;
                var info = (SessionInfoEx)Marshal.PtrToStructure(sessionBuffer, typeof(SessionInfoEx));
                if (!SessionIdentityAllowed(session, info.Level, info.Data.SessionId)) return false;
                string station = ObjectName(GetProcessWindowStation());
                inputDesktop = OpenInputDesktop(0, false, 1); // DESKTOP_READOBJECTS=1，不切换桌面。
                if (inputDesktop == IntPtr.Zero) return false;
                string desktop = ObjectName(inputDesktop);
                var foreground = GetForegroundWindow();
                string foregroundProcess = null;
                if (foreground != IntPtr.Zero) {
                    uint foregroundPid;
                    if (GetWindowThreadProcessId(foreground, out foregroundPid) == 0 || foregroundPid == 0) return false;
                    using (var process = Process.GetProcessById((int)foregroundPid)) {
                        foregroundProcess = process.ProcessName;
                    }
                }
                return DesktopEvidenceAllowed(info.Data.SessionState, unchecked((uint)info.Data.SessionFlags),
                    station, desktop, foreground != IntPtr.Zero, foregroundProcess);
            } catch { return false; }
            finally {
                if (inputDesktop != IntPtr.Zero) CloseDesktop(inputDesktop);
                if (sessionBuffer != IntPtr.Zero) WTSFreeMemory(sessionBuffer);
            }
        }
        public static long[] VisibleWindows(Process process) {
            var found = new HashSet<long>();
            int expectedPid = process.Id;
            EnumProc callback = delegate(IntPtr hwnd, IntPtr unused) {
                uint actualPid;
                GetWindowThreadProcessId(hwnd, out actualPid);
                if (actualPid == expectedPid && IsWindowVisible(hwnd) && GetWindow(hwnd, 4) == IntPtr.Zero)
                    found.Add(hwnd.ToInt64());
                return true;
            };
            // 只枚举持有 fixture 进程的线程，不枚举整个桌面或用户进程。
            foreach (ProcessThread thread in process.Threads) {
                try { EnumThreadWindows((uint)thread.Id, callback, IntPtr.Zero); }
                finally { thread.Dispose(); }
            }
            var result = new long[found.Count]; found.CopyTo(result); return result;
        }
        public static bool Matches(long hwnd, int expectedPid, string title) {
            uint actualPid;
            var handle = new IntPtr(hwnd);
            GetWindowThreadProcessId(handle, out actualPid);
            var text = new StringBuilder(256); GetWindowText(handle, text, text.Capacity);
            return actualPid == expectedPid && IsWindowVisible(handle) && text.ToString() == title;
        }
    }
}
'@
}

function Open-ABPinnedFile {
    param([string]$Path, [string]$ExpectedSha)
    Require-AB ($ExpectedSha -match '^[0-9a-fA-F]{64}$') 'AB_SHA_FORMAT'
    $full = [IO.Path]::GetFullPath($Path)
    Require-AB ([IO.File]::Exists($full)) 'AB_FILE_MISSING'
    Require-AB (([IO.File]::GetAttributes($full) -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'AB_FILE_REPARSE'
    $stream = [IO.File]::Open($full, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $hash = [Security.Cryptography.SHA256]::Create()
        try { $actual = ([BitConverter]::ToString($hash.ComputeHash($stream))).Replace('-', '').ToLowerInvariant() }
        finally { $hash.Dispose() }
        Require-AB ($actual -ceq $ExpectedSha.ToLowerInvariant()) 'AB_SHA_MISMATCH'
        $stream.Position = 0
        # 持有只读句柄至清理完成，禁止原地改写或替换本次固定二进制。
        return [pscustomobject]@{ Path = $full; Sha256 = $actual; Stream = $stream }
    }
    catch { $stream.Dispose(); throw }
}

function New-ABEnvironment {
    param([string]$DriverHome)
    return @{
        CUA_DRIVER_RS_HOME = $DriverHome
        CUA_DRIVER_RS_TELEMETRY_ENABLED = 'false'
        CUA_TELEMETRY_ENABLED = 'false'; CUA_TELEMETRY = '0'
        CUA_TELEMETRY_FORBID_NETWORK = '1'; DO_NOT_TRACK = '1'
        CUA_DRIVER_RS_UPDATE_CHECK = 'false'
    }
}

function Start-ABOwnedProcess {
    param([string]$Executable, [string[]]$Arguments, [hashtable]$Environment,
        [int]$StdoutLimit, [string]$Role)
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Executable
    $info.Arguments = (@($Arguments | ForEach-Object { ConvertTo-ABWindowsArgument $_ }) -join ' ')
    $info.WorkingDirectory = [IO.Path]::GetDirectoryName($Executable)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $info.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    foreach ($key in $Environment.Keys) { $info.EnvironmentVariables[$key] = $Environment[$key] }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    try {
        Require-AB ($process.Start()) 'AB_PROCESS_START'
        $null = $process.Handle
        $owner = [pscustomobject]@{
            Process = $process; Pid = $process.Id; StartTicks = [CuaReadonlyAB.Windows]::CreationTicks($process)
            Path = $Executable; Role = $Role; Cleaned = $false; ExitCode = $null
            Out = [CuaReadonlyAB.Pump]::new($process.StandardOutput, $StdoutLimit)
            Err = [CuaReadonlyAB.Pump]::new($process.StandardError, 0)
        }
        return $owner
    }
    catch {
        # Start 之后的登记失败也只能清理这个 Process 对象。
        try { if (-not $process.HasExited) { $process.Kill(); $null = $process.WaitForExit(3000) } } catch {}
        $process.Dispose()
        throw [InvalidOperationException]::new('AB_PROCESS_START')
    }
}

function Invoke-ABCheckStep {
    param([scriptblock]$Trace, [string]$Name, [scriptblock]$Operation)
    if ($null -eq $Trace) { & $Operation }
    else { & $Trace $Name $Operation }
}

function Assert-ABOwnedProcess {
    param($Owner, [scriptblock]$Trace = $null)
    Invoke-ABCheckStep $Trace ($Owner.Role + '.Identity') {
        Require-AB (-not $Owner.Process.HasExited) 'AB_PROCESS_EXITED'
        $Owner.Process.Refresh()
        Require-AB ($Owner.Process.Id -eq $Owner.Pid) 'AB_PROCESS_IDENTITY_PID'
        Require-AB ([CuaReadonlyAB.Windows]::CreationTicks($Owner.Process) -eq $Owner.StartTicks) 'AB_PROCESS_IDENTITY_START_TIME'
    }
    Invoke-ABCheckStep $Trace ($Owner.Role + '.ImagePath') {
        Require-AB ([string]::Equals([CuaReadonlyAB.Windows]::ImagePath($Owner.Process), $Owner.Path, [StringComparison]::OrdinalIgnoreCase)) 'AB_PROCESS_IDENTITY_IMAGE'
    }
}

function Stop-ABOwnedProcess {
    param($Owner, [bool]$Graceful)
    if ($null -eq $Owner -or $Owner.Cleaned) { return $true }
    $ok = $true
    try {
        if ($Owner.Process.HasExited) { $ok = $false }
        else {
            if ($Graceful) {
                $closed = $Owner.Process.CloseMainWindow()
                if (-not $closed -or -not $Owner.Process.WaitForExit(3000)) { $ok = $false }
            }
            if (-not $Owner.Process.HasExited) {
                $Owner.Process.Kill()
                Require-AB ($Owner.Process.WaitForExit(3000)) 'AB_PROCESS_CLEANUP_TIMEOUT'
            }
        }
        $code = $Owner.Process.ExitCode
        Require-AB (Test-ABInteger $code) 'AB_PROCESS_EXIT_CODE_MISSING'
        $Owner.ExitCode = $code
        if ($Graceful -and $code -ne 0) { $ok = $false }
        Require-AB ($Owner.Out.Completion.Wait(2000) -and $Owner.Err.Completion.Wait(2000)) 'AB_PROCESS_DRAIN_TIMEOUT'
    }
    catch { $ok = $false }
    finally {
        $Owner.Cleaned = $true
        $Owner.Process.Dispose()
    }
    return $ok
}

function Get-ABRemainingMs {
    param([Diagnostics.Stopwatch]$Clock, [int]$BudgetMs)
    $left = [long]$BudgetMs - $Clock.ElapsedMilliseconds
    Require-AB ($left -gt 0) 'AB_DEADLINE'
    return [int][Math]::Min([int]::MaxValue, $left)
}

function Invoke-ABCLI {
    param($PinnedDriver, [string[]]$Arguments, [AllowEmptyString()][string]$InputJson,
        [hashtable]$Environment, [int]$DeadlineMs, $Counters)
    $Counters.cli_attempts++
    $owner = $null
    $timer = [Diagnostics.Stopwatch]::StartNew()
    try {
        $owner = Start-ABOwnedProcess $PinnedDriver.Path $Arguments $Environment 8388608 'cli'
        $write = $owner.Process.StandardInput.WriteAsync($InputJson)
        Require-AB ($write.Wait((Get-ABRemainingMs $timer $DeadlineMs))) 'AB_CLI_STDIN_TIMEOUT'
        $owner.Process.StandardInput.Close()
        $left = Get-ABRemainingMs $timer $DeadlineMs
        Require-AB ($owner.Process.WaitForExit($left)) 'AB_CLI_TIMEOUT'
        $code = $owner.Process.ExitCode
        Require-AB (Test-ABInteger $code) 'AB_CLI_EXIT_CODE_MISSING'
        $Counters.cli_exit_codes++
        Assert-ABExitCode $code
        Require-AB ($owner.Out.Completion.Wait((Get-ABRemainingMs $timer $DeadlineMs))) 'AB_CLI_STDOUT_TIMEOUT'
        Require-AB ($owner.Err.Completion.Wait((Get-ABRemainingMs $timer $DeadlineMs))) 'AB_CLI_STDERR_TIMEOUT'
        Require-AB (-not $owner.Out.Overflow) 'AB_CLI_OUTPUT_LIMIT'
        $Counters.cli_zero_exits++
        $Counters.cli_stdout_chars += $owner.Out.Count
        $Counters.cli_stderr_chars += $owner.Err.Count
        return [pscustomobject]@{ Text = $owner.Out.Text; ElapsedMs = $timer.Elapsed.TotalMilliseconds }
    }
    finally {
        if ($null -ne $owner) {
            if (-not $owner.Process.HasExited) { $owner.Process.Kill() }
            Require-AB ($owner.Process.WaitForExit(3000)) 'AB_CLI_CLEANUP_TIMEOUT'
            # 不能为读取管道而无期限等待，尤其不等待 CLI 的后代进程树。
            try { $owner.Process.StandardOutput.Close(); $owner.Process.StandardError.Close() } catch {}
            $owner.Process.Dispose()
        }
    }
}

function Invoke-ABTool {
    param($Group, [string]$Tool, $Arguments, [bool]$Cleanup = $false)
    Require-AB ($Tool -cin $script:AllowedTools) 'AB_TOOL_NOT_ALLOWED'
    Assert-ABOwnedProcess $Group.Daemon
    $deadline = $CallTimeoutMs
    if (-not $Cleanup) { $deadline = [Math]::Min($deadline, (Get-ABRemainingMs $Group.Clock ($GroupTimeoutSeconds * 1000))) }
    $json = $Arguments | ConvertTo-Json -Compress -Depth 12
    $reply = Invoke-ABCLI $Group.Driver @('call', '--socket', $Group.Socket, $Tool) $json $Group.Environment $deadline $Group.Result
    try { $payload = ConvertFrom-Json -InputObject $reply.Text -ErrorAction Stop }
    catch { throw [InvalidOperationException]::new('AB_CLI_JSON') }
    Assert-ABNoImages $payload
    return [pscustomobject]@{ Payload = $payload; ElapsedMs = $reply.ElapsedMs }
}

function Assert-ABFixtureWindow {
    param($Fixture, [long]$WindowId, [scriptblock]$Trace = $null)
    Invoke-ABCheckStep $Trace 'Desktop' { Require-AB ([CuaReadonlyAB.Windows]::InteractiveDesktop()) 'AB_DESKTOP_NOT_INTERACTIVE' }
    Assert-ABOwnedProcess $Fixture $Trace
    Invoke-ABCheckStep $Trace 'FixtureWindows' {
        $handles = [CuaReadonlyAB.Windows]::VisibleWindows($Fixture.Process)
        Require-AB ($handles.Length -eq 1 -and $handles[0] -eq $WindowId -and
            [CuaReadonlyAB.Windows]::Matches($WindowId, $Fixture.Pid, $script:FixtureTitle)) 'AB_FIXTURE_WINDOW_IDENTITY'
    }
}

function Get-ABProcessMetrics {
    param($Owner, [scriptblock]$Trace = $null)
    Assert-ABOwnedProcess $Owner $Trace
    Invoke-ABCheckStep $Trace ($Owner.Role + '.Metrics') {
        return [ordered]@{
            pid = $Owner.Pid; private_bytes = $Owner.Process.PrivateMemorySize64
            handles = $Owner.Process.HandleCount; threads = $Owner.Process.Threads.Count
            cpu_ms = $Owner.Process.TotalProcessorTime.TotalMilliseconds
        }
    }
}

function Add-ABSample {
    param($Group, $Fixture, [string]$Phase, [int]$Completed)
    Assert-ABFixtureWindow $Fixture $Group.WindowId
    $Group.Result.samples.Add([ordered]@{
        phase = $Phase; observations_completed = $Completed
        elapsed_ms = $Group.Clock.Elapsed.TotalMilliseconds
        driver = Get-ABProcessMetrics $Group.Daemon
        fixture = Get-ABProcessMetrics $Fixture
    })
}

function Assert-ABCompleted {
    param($Result, [int]$ExpectedWarmup, [int]$ExpectedObservations, [int]$ExpectedPostSamples,
        [int]$ExpectedSampleEvery = 1)
    Require-AB ($ExpectedWarmup -gt 0 -and $ExpectedObservations -gt 0 -and
        $Result.warmup_completed -eq $ExpectedWarmup -and
        $Result.observations_completed -eq $ExpectedObservations -and
        $Result.end_session_verified -eq $true -and
        $Result.post_end_samples -eq $ExpectedPostSamples -and $ExpectedPostSamples -gt 0 -and
        $Result.cli_attempts -eq ($ExpectedWarmup + $ExpectedObservations + 3) -and
        $Result.cli_exit_codes -eq $Result.cli_attempts -and
        $Result.cli_zero_exits -eq $Result.cli_attempts -and
        $Result.cleanup_ok -eq $true -and
        (Test-ABInteger $Result.daemon_exit_code)) 'AB_GROUP_INCOMPLETE'
    Require-AB ($ExpectedSampleEvery -gt 0) 'AB_SAMPLE_SCHEDULE'
    $initial = @($Result.samples | Where-Object { $_.phase -ceq 'before_warmup' })
    $baseline = @($Result.samples | Where-Object { $_.phase -ceq 'after_warmup' })
    $regular = @($Result.samples | Where-Object { $_.phase -ceq 'observations' })
    $final = @($Result.samples | Where-Object { $_.phase -ceq 'before_end_session' })
    $post = @($Result.samples | Where-Object { $_.phase -ceq 'after_end_session' })
    Require-AB ($initial.Count -eq 1 -and $baseline.Count -eq 1 -and $final.Count -eq 1 -and
        $initial[0].observations_completed -eq 0 -and $baseline[0].observations_completed -eq 0 -and
        $final[0].observations_completed -eq $ExpectedObservations -and
        $regular.Count -eq [Math]::Floor($ExpectedObservations / $ExpectedSampleEvery) -and
        $post.Count -eq $ExpectedPostSamples -and
        $Result.samples.Count -eq (3 + $regular.Count + $post.Count)) 'AB_SAMPLE_SCHEDULE'
    for ($i = 0; $i -lt $regular.Count; $i++) {
        Require-AB ($regular[$i].observations_completed -eq (($i + 1) * $ExpectedSampleEvery)) 'AB_SAMPLE_SCHEDULE'
    }
    foreach ($sample in $post) {
        Require-AB ($sample.observations_completed -eq $ExpectedObservations) 'AB_SAMPLE_SCHEDULE'
    }
}

function Get-ABFailureCode {
    param($ErrorRecord)
    $message = $ErrorRecord.Exception.Message
    if ($message -cmatch '^AB_[A-Z_]+$') { return $message }
    return 'AB_RUNTIME_ERROR'
}

function Invoke-ABGroup {
    param($Driver, [string]$Label, [string]$Root, $Fixture, [long]$WindowId, [string]$Session, $Report)
    $dir = Join-Path $Root $Label
    $null = [IO.Directory]::CreateDirectory($dir)
    $manifestPath = Join-Path $dir 'manifest.json'
    $manifest = New-ABManifest $Fixture.Path
    Assert-ABManifest $manifest $Fixture.Path
    [IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
    $result = [ordered]@{
        role = $Label; driver_sha256 = $Driver.Sha256; status = 'FAIL'; failure_code = $null
        warmup_completed = 0; observations_completed = 0; end_session_verified = $false
        post_end_samples = 0; cli_attempts = 0; cli_exit_codes = 0; cli_zero_exits = 0
        cli_stdout_chars = 0L; cli_stderr_chars = 0L
        observed_elements_total = 0L; observed_tree_chars_total = 0L
        warmup_elapsed_ms_total = 0.0; observation_elapsed_ms_total = 0.0
        cleanup_ok = $false; daemon_exit_code = $null
        samples = [Collections.Generic.List[object]]::new()
    }
    $Report.groups.Add($result)
    $group = [pscustomobject]@{
        Driver = $Driver; Result = $result; Daemon = $null; WindowId = $WindowId
        Clock = [Diagnostics.Stopwatch]::StartNew()
        Socket = ('\\.\pipe\cortex-readonly-ab-' + [guid]::NewGuid().ToString('N'))
        Environment = New-ABEnvironment (Join-Path $dir 'driver-home')
    }
    $sessionAttempted = $false
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $request = New-ABStateArguments $Fixture.Pid $WindowId $Session
    try {
        $pidPath = Join-Path $dir 'daemon.pid'
        $group.Daemon = Start-ABOwnedProcess $Driver.Path @('serve', '--permission-mode', 'bounded',
            '--capability-manifest', $manifestPath, '--approve-capability-manifest',
            '--socket', $group.Socket, '--pid-file', $pidPath, '--no-overlay') $group.Environment 0 'daemon'
        $pipeReady = $false
        $startup = [Diagnostics.Stopwatch]::StartNew()
        while ($startup.ElapsedMilliseconds -lt 15000 -and -not $pipeReady) {
            Assert-ABOwnedProcess $group.Daemon
            $pipeReady = [CuaReadonlyAB.Windows]::PipeReady($group.Socket)
            if (-not $pipeReady) { Start-Sleep -Milliseconds 50 }
        }
        Require-AB $pipeReady 'AB_DAEMON_NOT_READY'
        $timeout = [Math]::Min($CallTimeoutMs, (Get-ABRemainingMs $startup 15000))
        $status = Invoke-ABCLI $Driver @('status', '--socket', $group.Socket, '--pid-file', $pidPath) '' $group.Environment $timeout $result
        Assert-ABStatus $status.Text
        Assert-ABFixtureWindow $Fixture $WindowId
        $sessionAttempted = $true
        $reply = Invoke-ABTool $group 'start_session' @{ session = $Session }
        Assert-ABSession $reply.Payload $Session $true
        Add-ABSample $group $Fixture 'before_warmup' 0
        for ($i = 1; $i -le ($Warmup + $Observations); $i++) {
            Assert-ABFixtureWindow $Fixture $WindowId
            $reply = Invoke-ABTool $group 'get_window_state' $request
            $counts = Assert-ABState $reply.Payload $Fixture.Pid $WindowId $seen
            $result.observed_elements_total += $counts.elements
            $result.observed_tree_chars_total += $counts.tree_chars
            if ($i -le $Warmup) { $result.warmup_elapsed_ms_total += $reply.ElapsedMs }
            else { $result.observation_elapsed_ms_total += $reply.ElapsedMs }
            $reply = $null
            if ($i -le $Warmup) {
                $result.warmup_completed++
                if ($i -eq $Warmup) { Add-ABSample $group $Fixture 'after_warmup' 0 }
            }
            else {
                $result.observations_completed++
                if ($result.observations_completed % $SampleEvery -eq 0) {
                    Add-ABSample $group $Fixture 'observations' $result.observations_completed
                }
            }
            if ($IntervalMs -gt 0 -and $i -lt ($Warmup + $Observations)) {
                Require-AB ((Get-ABRemainingMs $group.Clock ($GroupTimeoutSeconds * 1000)) -gt $IntervalMs) 'AB_DEADLINE'
                Start-Sleep -Milliseconds $IntervalMs
            }
        }
        Add-ABSample $group $Fixture 'before_end_session' $result.observations_completed
    }
    catch { $result.failure_code = Get-ABFailureCode $_ }
    finally {
        if ($sessionAttempted) {
            try {
                $reply = Invoke-ABTool $group 'end_session' @{ session = $Session } $true
                Assert-ABSession $reply.Payload $Session $false
                $result.end_session_verified = $true
                $reply = $null
                for ($i = 0; $i -lt $PostEndSamples; $i++) {
                    Start-Sleep -Milliseconds $PostEndIntervalMs
                    Add-ABSample $group $Fixture 'after_end_session' $result.observations_completed
                    $result.post_end_samples++
                }
            }
            catch { $result.failure_code = Get-ABFailureCode $_ }
        }
        $result.cleanup_ok = Stop-ABOwnedProcess $group.Daemon $false
        if ($null -ne $group.Daemon) { $result.daemon_exit_code = $group.Daemon.ExitCode }
    }
    try {
        Require-AB ($null -eq $result.failure_code) 'AB_GROUP_FAILED'
        Assert-ABCompleted $result $Warmup $Observations $PostEndSamples $SampleEvery
        $result.status = 'PASS'
    }
    catch {
        if ($null -eq $result.failure_code) { $result.failure_code = Get-ABFailureCode $_ }
    }
}

function Invoke-ABSelfTests {
    # 只构造内存对象；不初始化 native、打开文件、启动进程或枚举窗口。
    $checks = 0
    function Expect-ABReject {
        param([scriptblock]$Action, [string]$ExpectedCode)
        $rejected = $false
        try { $null = & $Action }
        catch {
            Require-AB ($_.Exception.Message -ceq $ExpectedCode) 'AB_SELFTEST_WRONG_REJECTION'
            $rejected = $true
        }
        Require-AB $rejected 'AB_SELFTEST_EXPECTED_REJECTION'
    }
    $controlProof = @{evidence_kind='control_build_only';base_commit='85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19';
        toolchain='1.98.0-x86_64-pc-windows-msvc';exit_code=0;binary_sha256=('a'*64);
        manifest_and_lock_unchanged=$true;control_source_unchanged=$true;candidate_source_and_binary_unchanged=$true}
    Assert-ABControlBuildProof $controlProof ('a'*64)
    $checks++
    foreach ($field in @('evidence_kind','base_commit','toolchain','exit_code','binary_sha256',
            'manifest_and_lock_unchanged','control_source_unchanged','candidate_source_and_binary_unchanged')) {
        $bad = $controlProof.Clone(); $bad[$field] = $null
        Expect-ABReject { Assert-ABControlBuildProof $bad ('a'*64) } 'AB_CONTROL_BUILD_PROOF'
        $checks++
    }
    foreach ($code in @($null, 1, -1, '0', $false)) {
        Expect-ABReject { Assert-ABExitCode $code } 'AB_CLI_EXIT_CODE'
        $checks++
    }
    Assert-ABExitCode 0
    $checks++
    Require-AB ((ConvertTo-ABWindowsArgument '') -ceq '""') 'AB_SELFTEST_QUOTE'
    Require-AB ((ConvertTo-ABWindowsArgument 'a b\') -ceq '"a b\\"') 'AB_SELFTEST_QUOTE'
    Require-AB ((ConvertTo-ABWindowsArgument 'a"b') -ceq '"a\"b"') 'AB_SELFTEST_QUOTE'
    $checks += 3
    Assert-ABStatus 'permission mode: bounded approved_at_startup=true valid=true'
    Expect-ABReject { Assert-ABStatus 'permission mode: bounded approved_at_startup=true invalid=true' } 'AB_DAEMON_NOT_READY'
    Expect-ABReject { Assert-ABStatus 'permission mode: unbounded approved_at_startup=true valid=true' } 'AB_DAEMON_NOT_READY'
    Expect-ABReject { Assert-ABStatus 'permission mode: bounded approved_at_startup=false valid=true' } 'AB_DAEMON_NOT_READY'
    $checks += 4
    $manifest = New-ABManifest 'C:\fixture\only.exe'
    Assert-ABManifest $manifest 'C:\fixture\only.exe'
    $manifest.allow.tools += 'click'
    Expect-ABReject { Assert-ABManifest $manifest 'C:\fixture\only.exe' } 'AB_MANIFEST_TOOLS'
    $manifest = New-ABManifest 'C:\fixture\only.exe'
    $manifest.resources.desktop = @{ display = 'all' }
    Expect-ABReject { Assert-ABManifest $manifest 'C:\fixture\only.exe' } 'AB_MANIFEST_RESOURCE'
    $checks += 3
    $request = New-ABStateArguments 123 456 's'
    Require-AB ($request.since -eq $null -and $request.include_screenshot -eq $false -and
        $request.tree_format -ceq 'both' -and $request.pid -eq 123 -and $request.window_id -eq 456) 'AB_SELFTEST_REQUEST'
    $checks++
    $good = '{"pid":123,"window_id":456,"window_title":"Cortex Computer Use Fixture","snapshot_id":"s1","tree_format":"both","tree_markdown":"test-only","elements_complete":true,"truncated":false,"total_element_count":1,"returned_element_count":1,"elements":[{"element_token":"s1:0"}]}'
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $null = Assert-ABState (ConvertFrom-Json $good) 123 456 $seen
    Expect-ABReject { Assert-ABState (ConvertFrom-Json $good) 123 456 $seen } 'AB_SNAPSHOT_REUSED'
    $checks += 2
    $rejectCodes = @{
        pid = 'AB_STATE_PID'; window_id = 'AB_STATE_HWND'; snapshot_id = 'AB_SNAPSHOT_MISSING'
        tree_markdown = 'AB_TREE_MISSING'; elements_complete = 'AB_TREE_INCOMPLETE'
        truncated = 'AB_TREE_INCOMPLETE'; total_element_count = 'AB_ELEMENT_COUNT'
        element_token = 'AB_ELEMENT_TOKEN'; image = 'AB_IMAGE_CONTENT'
        resource = 'AB_IMAGE_PAYLOAD'; error = 'AB_STATE_ERROR'; image_text = 'AB_IMAGE_TEXT'
    }
    foreach ($case in $rejectCodes.Keys) {
        $bad = ConvertFrom-Json $good
        switch ($case) {
            'pid' { $bad.pid = 124 }
            'window_id' { $bad.window_id = 457 }
            'snapshot_id' { $bad.snapshot_id = '' }
            'tree_markdown' { $bad.tree_markdown = '' }
            'elements_complete' { $bad.elements_complete = $false }
            'truncated' { $bad.truncated = $true }
            'total_element_count' { $bad.total_element_count = 2 }
            'element_token' { $bad.elements[0].element_token = 'other:0' }
            'image' { $bad | Add-Member -NotePropertyName content -NotePropertyValue @(@{ type = 'image'; data = 'hidden' }) }
            'resource' { $bad | Add-Member -NotePropertyName resource -NotePropertyValue @{ blob = 'hidden' } }
            'error' { $bad | Add-Member -NotePropertyName error -NotePropertyValue @{ message = 'hidden' } }
            'image_text' { $bad.tree_markdown = 'data:image/png;base64,hidden' }
        }
        $fresh = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        Expect-ABReject { Assert-ABState $bad 123 456 $fresh } $rejectCodes[$case]
        $checks++
    }
    Assert-ABSession ([pscustomobject]@{ session = 's'; active = $true }) 's' $true
    Expect-ABReject { Assert-ABSession ([pscustomobject]@{ session = 'wrong'; active = $true }) 's' $true } 'AB_SESSION_IDENTITY'
    Expect-ABReject { Assert-ABSession ([pscustomobject]@{ session = 's'; active = 'false' }) 's' $false } 'AB_SESSION_ACTIVE'
    $checks += 3
    $complete = @{
        warmup_completed = 1; observations_completed = 2; end_session_verified = $true
        post_end_samples = 1; cli_attempts = 6; cli_exit_codes = 6; cli_zero_exits = 6
        cleanup_ok = $true; daemon_exit_code = -1
        samples = @(
            @{ phase = 'before_warmup'; observations_completed = 0 },
            @{ phase = 'after_warmup'; observations_completed = 0 },
            @{ phase = 'observations'; observations_completed = 1 },
            @{ phase = 'observations'; observations_completed = 2 },
            @{ phase = 'before_end_session'; observations_completed = 2 },
            @{ phase = 'after_end_session'; observations_completed = 2 }
        )
    }
    Assert-ABCompleted $complete 1 2 1
    foreach ($case in @('zero', 'exit', 'nonzero', 'end', 'cleanup', 'missing_daemon_exit')) {
        $bad = $complete.Clone()
        switch ($case) {
            'zero' { $bad.observations_completed = 0 }
            'exit' { $bad.cli_exit_codes = 4 }
            'nonzero' { $bad.cli_zero_exits = 4 }
            'end' { $bad.end_session_verified = $false }
            'cleanup' { $bad.cleanup_ok = $false }
            'missing_daemon_exit' { $bad.daemon_exit_code = $null }
        }
        Expect-ABReject { Assert-ABCompleted $bad 1 2 1 } 'AB_GROUP_INCOMPLETE'
        $checks++
    }
    Expect-ABReject { Assert-ABCompleted $complete 1 0 1 } 'AB_GROUP_INCOMPLETE'
    Expect-ABReject { Assert-ABCompleted $complete 0 2 1 } 'AB_GROUP_INCOMPLETE'
    $bad = $complete.Clone()
    $bad.samples = @($bad.samples | Where-Object { $_.phase -cne 'after_warmup' })
    Expect-ABReject { Assert-ABCompleted $bad 1 2 1 } 'AB_SAMPLE_SCHEDULE'
    $checks++
    Expect-ABReject { Assert-ABNoImages @{ resource = @{ blob = '0' } } } 'AB_IMAGE_PAYLOAD'
    Expect-ABReject { Assert-ABNoImages @{ content = @(@{ type = 'text'; text = 'data:image/png;base64,test' }) } } 'AB_IMAGE_TEXT'
    $environment = New-ABEnvironment 'C:\isolated\driver-home'
    Require-AB ($environment.CUA_DRIVER_RS_UPDATE_CHECK -ceq 'false' -and
        $environment.CUA_DRIVER_RS_TELEMETRY_ENABLED -ceq 'false' -and
        $environment.CUA_TELEMETRY_FORBID_NETWORK -ceq '1') 'AB_SELFTEST_ENV'
    $checks += 5
    Write-Output ('SOURCE_SELFTEST_PASS checks=' + $checks + ' scope=' + $script:Scope)
}

function Invoke-ABDesktopClassificationSelfTests {
    # Initialize-ABNative 只编译声明；这里仅调用纯分类器，不执行任何 P/Invoke。
    $checks = 0
    $base = @{ state=0; flags=1; station='WinSta0'; desktop='Default'; foreground=$false; process=$null }
    $cases = [Collections.Generic.List[object]]::new()
    $cases.Add(@{ value=$base.Clone(); allowed=$true; name='DEFAULT_FOREGROUND_ZERO' })
    $lower = $base.Clone(); $lower.station='winsta0'; $lower.desktop='default'
    $cases.Add(@{ value=$lower; allowed=$true; name='CASE_INSENSITIVE_DEFAULT' })
    $visible = $base.Clone(); $visible.foreground=$true; $visible.process='fixture'
    $cases.Add(@{ value=$visible; allowed=$true; name='ORDINARY_FOREGROUND' })
    foreach ($field in @('state', 'flags', 'station', 'desktop')) {
        $values = switch ($field) {
            'state' { @(1, 4, -1) }
            'flags' { @(0, 2, [uint32]::MaxValue) }
            'station' { @($null, '', 'Service-0x0') }
            'desktop' { @($null, '', 'Winlogon', 'LockApp', 'Unknown') }
        }
        foreach ($value in $values) {
            $bad = $base.Clone(); $bad[$field] = $value
            $cases.Add(@{ value=$bad; allowed=$false; name=('REJECT_' + $field.ToUpperInvariant()) })
        }
    }
    foreach ($name in @('LockApp', 'lockapp', 'LogonUI', 'Winlogon', '', ' ', $null)) {
        $bad = $base.Clone(); $bad.foreground=$true; $bad.process=$name
        $cases.Add(@{ value=$bad; allowed=$false; name='REJECT_SECURE_OR_UNKNOWN_FOREGROUND' })
    }
    foreach ($case in $cases) {
        $v = $case.value
        $actual = [CuaReadonlyAB.Windows]::DesktopEvidenceAllowed([int]$v.state, [uint32]$v.flags,
            [string]$v.station, [string]$v.desktop, [bool]$v.foreground, [string]$v.process)
        Require-AB ($actual -eq $case.allowed) ('AB_DESKTOP_CLASSIFICATION_' + $case.name)
        $checks++
    }
    foreach ($case in @(@(1,1,1,$true), @(0,1,0,$false), @(1,0,1,$false), @(1,2,1,$false), @(1,1,2,$false))) {
        $actual = [CuaReadonlyAB.Windows]::SessionIdentityAllowed($case[0], $case[1], $case[2])
        Require-AB ($actual -eq $case[3]) 'AB_DESKTOP_CLASSIFICATION_SESSION_IDENTITY'
        $checks++
    }
    # 仅检查本地类型布局，避免把 WTSINFOEXW 的 8 字节对齐误写为偏移 4。
    $level1 = [CuaReadonlyAB.Windows].GetNestedType('SessionInfoLevel1', [Reflection.BindingFlags]::NonPublic)
    $infoEx = [CuaReadonlyAB.Windows].GetNestedType('SessionInfoEx', [Reflection.BindingFlags]::NonPublic)
    # PS5.1 对 RuntimeType 会误选 SizeOf(object)，显式选择 Type 重载。
    $sizeOf = [Runtime.InteropServices.Marshal].GetMethod('SizeOf', [type[]]@([type]))
    Require-AB ($sizeOf.Invoke($null, [object[]]@($level1)) -eq 224) 'AB_DESKTOP_CLASSIFICATION_LEVEL_SIZE'
    Require-AB ($sizeOf.Invoke($null, [object[]]@($infoEx)) -eq 232) 'AB_DESKTOP_CLASSIFICATION_INFO_SIZE'
    Require-AB ([Runtime.InteropServices.Marshal]::OffsetOf($infoEx, 'Data').ToInt32() -eq 8) 'AB_DESKTOP_CLASSIFICATION_INFO_ALIGNMENT'
    $checks += 3
    Write-Output ('DESKTOP_CLASSIFICATION_SELFTEST_PASS checks=' + $checks + ' native_calls=0 foreground_zero_default=allowed')
}

# 长测只复用函数；导入阶段不得初始化 native 或触碰进程、窗口及运行目录。
if ($SourceOnly) { return }
if ($SelfTest) { Invoke-ABSelfTests; return }

$report = [ordered]@{
    schema_version = 1; scope = $script:Scope; status = 'FAIL'
    investigation_only = $true; product_signature_gate_passed = $false
    memory_steady_state_asserted = $false; failure_code = $null
    reference_kind = $PSCmdlet.ParameterSetName; reference_build_proof_sha256 = $null
    warmup = $Warmup; observations = $Observations; sample_every = $SampleEvery
    interval_ms = $IntervalMs; call_timeout_ms = $CallTimeoutMs
    group_timeout_seconds = $GroupTimeoutSeconds; post_end_samples = $PostEndSamples
    fixture_sha256 = $null; fixture_exit_code = $null; fixture_cleanup_ok = $false
    groups = [Collections.Generic.List[object]]::new()
}
$pins = [Collections.Generic.List[object]]::new()
$fixture = $null
$outputStream = $null
try {
    Require-AB ($env:OS -ceq 'Windows_NT') 'AB_WINDOWS_REQUIRED'
    if ($PSCmdlet.ParameterSetName -eq 'MatchedControl') {
        $referencePath = $ControlDriverPath; $referenceSha = $ControlExpectedSha256; $referenceLabel = 'matched_control'
        $proof = Get-Content -LiteralPath $ControlBuildProofPath -Raw -Encoding UTF8 | ConvertFrom-Json
        Assert-ABControlBuildProof $proof $referenceSha
        $report.reference_build_proof_sha256 = (Get-FileHash -LiteralPath $ControlBuildProofPath -Algorithm SHA256).Hash.ToLowerInvariant()
    } else {
        Require-AB ($OfficialExpectedSha256.ToLowerInvariant() -ceq $script:PinnedOfficialSha) 'AB_OFFICIAL_PIN'
        $referencePath = $OfficialDriverPath; $referenceSha = $OfficialExpectedSha256; $referenceLabel = 'official'
    }
    Require-AB ($SampleEvery -le $Observations) 'AB_SAMPLE_STEP'
    $fullOutput = [IO.Path]::GetFullPath($OutputPath)
    Require-AB (-not [IO.File]::Exists($fullOutput) -and -not [IO.Directory]::Exists($fullOutput)) 'AB_OUTPUT_EXISTS'
    $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($fullOutput))
    $outputStream = [IO.File]::Open($fullOutput, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $official = Open-ABPinnedFile $referencePath $referenceSha
    $pins.Add($official)
    $candidate = Open-ABPinnedFile $CandidateDriverPath $CandidateExpectedSha256
    $pins.Add($candidate)
    $sourceFixture = Open-ABPinnedFile $FixturePath $FixtureExpectedSha256
    $pins.Add($sourceFixture)
    Require-AB (-not [string]::Equals($official.Path, $candidate.Path, [StringComparison]::OrdinalIgnoreCase)) 'AB_DRIVER_PATHS_EQUAL'
    if ($PSCmdlet.ParameterSetName -ne 'MatchedControl') {
        $signature = Get-AuthenticodeSignature -LiteralPath $official.Path
        Require-AB ($signature.Status.ToString() -ceq 'Valid') 'AB_OFFICIAL_SIGNATURE'
    }
    $runRoot = Join-Path $PSScriptRoot ('runs\' + [guid]::NewGuid().ToString('N'))
    $null = [IO.Directory]::CreateDirectory($runRoot)
    $fixtureCopy = Join-Path $runRoot ('fixture-' + [guid]::NewGuid().ToString('N') + '.exe')
    $copy = [IO.File]::Open($fixtureCopy, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $sourceFixture.Stream.CopyTo($copy) } finally { $copy.Dispose() }
    $fixturePin = Open-ABPinnedFile $fixtureCopy $FixtureExpectedSha256
    $pins.Add($fixturePin)
    $report.fixture_sha256 = $fixturePin.Sha256
    Initialize-ABNative
    Require-AB ([CuaReadonlyAB.Windows]::InteractiveDesktop()) 'AB_DESKTOP_NOT_INTERACTIVE'
    $fixtureTimeoutMs = 2 * $GroupTimeoutSeconds * 1000 +
        2 * $PostEndSamples * $PostEndIntervalMs + 2 * $CallTimeoutMs + 90000
    $fixture = Start-ABOwnedProcess $fixtureCopy @('--no-secure-field', '--exit-timeout-ms', [string]$fixtureTimeoutMs) @{} 0 'fixture'
    $fixture.Process.StandardInput.Close()
    $windowId = 0L
    $startup = [Diagnostics.Stopwatch]::StartNew()
    while ($startup.ElapsedMilliseconds -lt 15000 -and $windowId -eq 0) {
        Assert-ABOwnedProcess $fixture
        $handles = [CuaReadonlyAB.Windows]::VisibleWindows($fixture.Process)
        Require-AB ($handles.Length -le 1) 'AB_FIXTURE_WINDOW_COUNT'
        if ($handles.Length -eq 1) { $windowId = $handles[0] }
        else { Start-Sleep -Milliseconds 50 }
    }
    Require-AB ($windowId -ne 0) 'AB_FIXTURE_WINDOW_TIMEOUT'
    Assert-ABFixtureWindow $fixture $windowId
    $sharedSession = 'readonly-ab-' + [guid]::NewGuid().ToString('N')
    Invoke-ABGroup $official $referenceLabel $runRoot $fixture $windowId $sharedSession $report
    Invoke-ABGroup $candidate 'candidate' $runRoot $fixture $windowId $sharedSession $report
    Require-AB ($report.groups.Count -eq 2 -and @($report.groups | Where-Object { $_.status -cne 'PASS' }).Count -eq 0) 'AB_GROUP_FAILED'
}
catch { $report.failure_code = Get-ABFailureCode $_ }
finally {
    $report.fixture_cleanup_ok = Stop-ABOwnedProcess $fixture $true
    if ($null -ne $fixture) { $report.fixture_exit_code = $fixture.ExitCode }
    foreach ($pin in $pins) { $pin.Stream.Dispose() }
    if ($null -eq $report.failure_code -and $report.fixture_cleanup_ok -eq $true -and
        (Test-ABInteger $report.fixture_exit_code) -and $report.fixture_exit_code -eq 0 -and
        $report.groups.Count -eq 2 -and @($report.groups | Where-Object { $_.status -cne 'PASS' }).Count -eq 0) {
        $report.status = 'PASS'
    }
    if ($null -ne $outputStream) {
        try {
            $writer = [IO.StreamWriter]::new($outputStream, [Text.UTF8Encoding]::new($true))
            try { $writer.Write(($report | ConvertTo-Json -Depth 12)); $writer.Flush() }
            finally { $writer.Dispose() }
        }
        finally { $outputStream.Dispose() }
    }
}
if ($report.status -cne 'PASS') { Write-Output ('AB_FAIL scope=' + $script:Scope); exit 1 }
Write-Output ('AB_PASS scope=' + $script:Scope)
