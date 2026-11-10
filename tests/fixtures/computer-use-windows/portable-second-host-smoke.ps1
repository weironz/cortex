<#
.SYNOPSIS
    在第二台 Windows 上运行预先构建的 Cortex Computer Use 测试程序。
.DESCRIPTION
    不构建程序，也不使用 Cargo。请从第一台机器通过独立渠道取得测试程序和
    fixture 和 VC 运行库的 SHA-256；Cua 0.38.0 的六个发布文件及其哈希固定在脚本内。
    测试程序和 fixture 在运行前复制到独立临时目录，并再次校验和锁定。
    仅验证路径、哈希、PowerShell 语法时使用 -ValidateOnly：不启动进程或
    创建结果文件，也不能证明测试存在、依赖齐全或桌面可用。实际运行需要
    Windows x64、WPF 及测试程序所需的系统运行库。

    安全阶段使用 --message 初始化固定的非敏感基线，并将已校验的 fixture
    哈希及同一基线传给测试程序。若所给测试程序没有这两个精确的 ignored
    测试，0 tests 会被判为失败。

    结果只证明所给二进制和这台机器上的本次运行；不证明源码对应或 GA。
.EXAMPLE
    powershell.exe -NoProfile -File .\portable-second-host-smoke.ps1 `
      -TestExePath 'D:\transfer\cortex-local-tests.exe' `
      -FixtureExePath 'D:\transfer\cortex-computer-use-soak-fixture.exe' `
      -VCRuntimePath 'D:\transfer\vcruntime140.dll' `
      -CuaDirectory 'D:\transfer\cua-0.38.0' `
      -ExpectedTestSha256 '<第一台机器独立提供的64位十六进制>' `
      -ExpectedFixtureSha256 '<第一台机器独立提供的64位十六进制>' `
      -ExpectedVCRuntimeSha256 '<第一台机器独立提供的64位十六进制>' `
      -OutputPath 'D:\transfer\second-host-result.json' -ValidateOnly
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$TestExePath,
    [Parameter(Mandatory = $true)][string]$FixtureExePath,
    [Parameter(Mandatory = $true)][string]$VCRuntimePath,
    [Parameter(Mandatory = $true)][string]$CuaDirectory,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedTestSha256,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedFixtureSha256,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedVCRuntimeSha256,
    [Parameter(Mandatory = $true)][string]$OutputPath,
    [ValidateRange(60, 21600)][int]$Seconds = 60,
    [ValidateRange(100, 60000)][int]$IntervalMs = 5000,
    [switch]$ValidateOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$secureName = 'computer::real::tests::cua_secure_fixture_requires_handoff_without_input'
$soakName = 'computer::real::tests::cua_fixture_long_running_soak'
$secureBaseline = 'Cortex-secure-fixture-baseline'
$cuaHashes = [ordered]@{
    'cua-driver.exe' = 'ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2'
    'cua-driver-uia.exe' = '69aa6ea0049ce59dfd14593be77c0632f12f6eda3d4a2f4bab1a00e3a72a9754'
    'cua-cursor-theme.exe' = 'ffaf56a62b0f3bdaf65f2bc30612e5d6ed408362d4868d6f20e8db93be851f0c'
    'cua_driver_sdk.dll' = '68c5540cc99eeb725352db3f3a944679f9d6e1757923b2d8d8ee441b39cd78d9'
    'cua_driver_node_runtime.node' = '3306eb003a0c57c0c0f58a488fb9cc052630fb800a0db611d3459a76531aec87'
    'cua_driver_abi.h' = 'c17169f41da321baa5e7e953323c3ad660b00790176ba381e93189fba3506587'
}

function Resolve-LocalPath {
    param([string]$Value, [bool]$MustExist, [bool]$Directory)
    if ([string]::IsNullOrWhiteSpace($Value) -or $Value -notmatch '^[A-Za-z]:\\') {
        throw "必须指定本地磁盘上的绝对路径：$Value"
    }
    $full = [IO.Path]::GetFullPath($Value)
    $drive = New-Object System.IO.DriveInfo ([IO.Path]::GetPathRoot($full))
    if ($drive.DriveType -notin @([IO.DriveType]::Fixed, [IO.DriveType]::Removable, [IO.DriveType]::Ram)) {
        throw "路径必须位于本地磁盘：$full"
    }
    if ($MustExist) {
        $type = if ($Directory) { 'Container' } else { 'Leaf' }
        if (-not (Test-Path -LiteralPath $full -PathType $type)) {
            throw "找不到所需路径：$full"
        }
        $full = (Resolve-Path -LiteralPath $full).ProviderPath
    }
    return $full
}

function Open-VerifiedInput {
    param([string]$Path, [string]$Expected, [System.Collections.Generic.List[System.IO.FileStream]]$Locks)
    # 持有只读共享句柄直到所有子进程退出，避免校验后替换文件。
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $Locks.Add($stream)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $actual = ([BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
    }
    finally { $sha.Dispose() }
    if ($actual -ne $Expected.ToLowerInvariant()) {
        throw "SHA-256 不匹配：$Path；实际 $actual"
    }
    return $actual
}

function Get-PortableSystemPath {
    # 本机会话保存会用系统 PowerShell 设置私有文件 ACL；仅 System32 不含该 exe。
    # 不继承开发机 PATH，防止另一个同名程序混入实际宿主接线。
    $system32 = [IO.Path]::Combine($env:SystemRoot, 'System32')
    $systemPowerShell = [IO.Path]::Combine($system32, 'WindowsPowerShell', 'v1.0')
    if (-not [IO.File]::Exists((Join-Path $systemPowerShell 'powershell.exe'))) {
        throw '找不到设置私有会话 ACL 所需的系统 Windows PowerShell。'
    }
    return [string]::Join(';', @($system32, $systemPowerShell))
}

function Get-CapabilityManifestHash {
    param([byte[]]$Bytes)
    # 固定 Cua 0.38/v3 与 Cortex 使用域分隔摘要，不能拿裸文件 SHA-256 比较。
    $prefix = [Text.Encoding]::UTF8.GetBytes("cua-driver-capability-manifest-v3`0")
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString(
            $sha.ComputeHash([byte[]]($prefix + $Bytes))
        )).Replace('-', '').ToLowerInvariant()
    }
    finally { $sha.Dispose() }
}

function Get-HarnessCheck {
    param([string]$Text, [string]$Name, [int]$ExitCode)
    $start = [regex]::Matches($Text, '(?m)^running 1 test\r?$').Count
    $namePattern = '(?m)^test ' + [regex]::Escape($Name) + ' \.\.\. ok\r?$'
    $named = [regex]::Matches($Text, $namePattern).Count
    $summary = [regex]::Matches($Text, '(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;.*\r?$').Count
    $runs = [regex]::Matches($Text, '(?m)^running [0-9]+ tests?\r?$').Count
    $summaries = [regex]::Matches($Text, '(?m)^test result: ').Count
    return [ordered]@{
        exitCode = $ExitCode
        runningOneLines = $start
        namedPassedLines = $named
        onePassedSummaryLines = $summary
        totalRunLines = $runs
        totalSummaryLines = $summaries
        passed = ($ExitCode -eq 0 -and $start -eq 1 -and $named -eq 1 -and $summary -eq 1 -and $runs -eq 1 -and $summaries -eq 1)
    }
}

function Test-HarnessParser {
    $manifestDigest = Get-CapabilityManifestHash -Bytes ([Text.Encoding]::UTF8.GetBytes('{"version":3}'))
    if ($manifestDigest -ne '6cbfac6d63baccb0fe1c38af025b800719186c07c5b3741edbd041ce196f6fcf') {
        throw 'Capability manifest 摘要未遵循固定 Cua v3 域分隔格式。'
    }
    # 这些只是字符串检查，不执行任何测试程序或桌面进程。
    $one = "running 1 test`ntest $secureName ... ok`ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.1s`n"
    if (-not (Get-HarnessCheck -Text $one -Name $secureName -ExitCode 0).passed) { throw '测试结果解析器拒绝了合法的单测试结果。' }
    $bad = @(
        "running 0 tests`ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.0s`n",
        ($one + $one),
        $one.Replace($secureName, 'another::test'),
        $one.Replace('0 failed', '1 failed'),
        $one.Replace('running 1 test', 'running 2 tests')
    )
    foreach ($sample in $bad) {
        if ((Get-HarnessCheck -Text $sample -Name $secureName -ExitCode 0).passed) { throw '测试结果解析器错误接受了无效结果。' }
    }
    if ((Get-HarnessCheck -Text $one -Name $secureName -ExitCode 1).passed) { throw '测试结果解析器错误接受了非零退出码。' }
}

function Get-SafeLogEvidence {
    param([string]$Path)
    # 只保存限长片段的分类结果，不持久化界面文字、输入内容或原始日志。
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    try {
        $length = $stream.Length
        $count = [int][Math]::Min($length, 65536)
        $null = $stream.Seek(-$count, [IO.SeekOrigin]::End)
        $buffer = New-Object byte[] $count
        $read = $stream.Read($buffer, 0, $count)
        $tail = [Text.Encoding]::UTF8.GetString($buffer, 0, $read)
        # 仅保留已知测试模块的文件名与数字位置；完整 panic 消息可能含界面值。
        $panicSite = [regex]::Match($tail, 'panicked at [^\r\n]*\b(computer(?:_http_fixture_test|_portable_fixture_test)?\.rs):([0-9]{1,7}):([0-9]{1,7})\b')
        $contractTags = @()
        foreach ($entry in @{
            attached_manifest = '附着 manifest 必须只包含唯一 fixture 和最小动作工具'
            owned_config = '附着必须通过产品 owned/pin/签名/bounded 校验'
            private_pipe = '拒绝非本次 portable 专用 pipe'
            fixture_filename = '不接受其他应用作为'
            fixture_process = '父脚本 fixture 原进程已退出或不可验证'
            temp_volume = 'portable 私有 authority 与宿主状态必须位于 C 盘临时目录'
        }.GetEnumerator()) {
            if ($tail.Contains($entry.Value)) { $contractTags += $entry.Key }
        }
        return [ordered]@{
            file = [IO.Path]::GetFileName($Path)
            totalBytes = $length
            inspectedBytes = $read
            panic = ($tail -match '(?i)panicked at|assertion .*failed')
            panicFile = $(if ($panicSite.Success) { $panicSite.Groups[1].Value } else { $null })
            panicLine = $(if ($panicSite.Success) { [int]$panicSite.Groups[2].Value } else { $null })
            panicColumn = $(if ($panicSite.Success) { [int]$panicSite.Groups[3].Value } else { $null })
            timedOut = ($tail -match '(?i)timed?\s*out|deadline')
            accessDenied = ($tail -match '(?i)access (is )?denied|os error 5')
            missingRuntime = ($tail -match '(?i)0xc0000135|STATUS_DLL_NOT_FOUND')
            handoff = ($tail -match '(?i)policyhandoff|secure.?input|secure.?field')
            missingTrustedHostContext = $tail.Contains('写动作缺少可信宿主主体/调用上下文')
            portableContractTags = @($contractTags | Sort-Object)
        }
    }
    finally { $stream.Dispose() }
}

function Initialize-OwnedProcessSupport {
    # 子进程先挂起、加入专用 job，再恢复，避免 daemon 在加入 job 之前派生子进程。
    # 关闭 job 只结束这次创建的进程树；不会按名称或 PID 扫描并终止别人的进程。
    if ('CortexPortableSmoke.OwnedJob' -as [type]) {
        throw '辅助进程类型已加载；请用新的 Windows PowerShell 5.1 进程运行本脚本。'
    }
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

namespace CortexPortableSmoke {
    [StructLayout(LayoutKind.Sequential)] struct BasicLimit {
        public long PerProcessUserTimeLimit, PerJobUserTimeLimit;
        public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize, MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint PriorityClass, SchedulingClass;
    }
    [StructLayout(LayoutKind.Sequential)] struct IoCounters {
        public ulong ReadOperationCount, WriteOperationCount, OtherOperationCount;
        public ulong ReadTransferCount, WriteTransferCount, OtherTransferCount;
    }
    [StructLayout(LayoutKind.Sequential)] struct ExtendedLimit {
        public BasicLimit BasicLimitInformation;
        public IoCounters IoInfo;
        public UIntPtr ProcessMemoryLimit, JobMemoryLimit, PeakProcessMemoryUsed, PeakJobMemoryUsed;
    }
    [StructLayout(LayoutKind.Sequential)] struct SecurityAttributes {
        public int Length;
        public IntPtr SecurityDescriptor;
        [MarshalAs(UnmanagedType.Bool)] public bool InheritHandle;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] struct StartupInfo {
        public int cb;
        public string reserved, desktop, title;
        public uint x, y, xSize, ySize, xCountChars, yCountChars, fillAttribute, flags;
        public ushort showWindow, reserved2;
        public IntPtr reserved2Ptr, stdInput, stdOutput, stdError;
    }
    [StructLayout(LayoutKind.Sequential)] struct StartupInfoEx {
        public StartupInfo startupInfo;
        public IntPtr attributeList;
    }
    [StructLayout(LayoutKind.Sequential)] struct ProcessInfo {
        public IntPtr process, thread;
        public uint processId, threadId;
    }
    public sealed class OwnedProcess : IDisposable {
        internal IntPtr Handle;
        public int Id { get; internal set; }
        public bool Wait(int milliseconds) { return Native.WaitForSingleObject(Handle, (uint)milliseconds) == 0; }
        public int ExitCode {
            get { uint result; if (!Native.GetExitCodeProcess(Handle, out result)) throw new Win32Exception(); return unchecked((int)result); }
        }
        public void Stop() {
            if (!Wait(0) && !Native.TerminateProcess(Handle, 1)) {
                int error = Marshal.GetLastWin32Error();
                if (!Wait(0)) throw new Win32Exception(error);
            }
            if (!Wait(5000)) throw new InvalidOperationException("Owned process did not exit");
        }
        public void Dispose() { if (Handle != IntPtr.Zero) { Native.CloseHandle(Handle); Handle = IntPtr.Zero; } }
    }
    public sealed class OwnedJob : IDisposable {
        IntPtr handle;
        public OwnedJob() {
            handle = Native.CreateJobObject(IntPtr.Zero, null);
            if (handle == IntPtr.Zero) throw new Win32Exception();
            ExtendedLimit limits = new ExtendedLimit();
            limits.BasicLimitInformation.LimitFlags = 0x2000;
            int size = Marshal.SizeOf(typeof(ExtendedLimit));
            IntPtr data = Marshal.AllocHGlobal(size);
            try {
                Marshal.StructureToPtr(limits, data, false);
                if (!Native.SetInformationJobObject(handle, 9, data, (uint)size)) throw new Win32Exception();
            } catch { Native.CloseHandle(handle); handle = IntPtr.Zero; throw; }
            finally { Marshal.FreeHGlobal(data); }
        }
        static string Quote(string value) {
            StringBuilder result = new StringBuilder("\"");
            int slash = 0;
            foreach (char c in value) {
                if (c == '\\') { slash++; continue; }
                if (c == '"') { result.Append('\\', slash * 2 + 1); result.Append(c); }
                else { result.Append('\\', slash); result.Append(c); }
                slash = 0;
            }
            result.Append('\\', slash * 2); result.Append('"'); return result.ToString();
        }
        public OwnedProcess Start(string exe, string[] args, string cwd, string log, bool hidden, IDictionary<string,string> env) {
            SecurityAttributes sa = new SecurityAttributes(); sa.Length = Marshal.SizeOf(typeof(SecurityAttributes)); sa.InheritHandle = true;
            IntPtr output = Native.CreateFile(log, 0x40000000, 1, ref sa, 1, 0x80, IntPtr.Zero);
            if (output == new IntPtr(-1)) throw new Win32Exception();
            IntPtr input = IntPtr.Zero, environment = IntPtr.Zero, attributes = IntPtr.Zero, handleList = IntPtr.Zero;
            ProcessInfo pi = new ProcessInfo();
            bool started = false, resumed = false, attributesReady = false;
            Exception startupError = null;
            try {
                input = Native.CreateFile("NUL", 0x80000000, 3, ref sa, 3, 0x80, IntPtr.Zero);
                if (input == new IntPtr(-1)) throw new Win32Exception();
                List<string> variables = new List<string>();
                foreach (KeyValuePair<string,string> item in env) variables.Add(item.Key + "=" + item.Value);
                variables.Sort(StringComparer.OrdinalIgnoreCase);
                environment = Marshal.StringToHGlobalUni(String.Join("\0", variables.ToArray()) + "\0\0");
                IntPtr attributesSize = IntPtr.Zero;
                Native.InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref attributesSize);
                if (attributesSize == IntPtr.Zero) throw new Win32Exception();
                attributes = Marshal.AllocHGlobal(attributesSize);
                if (!Native.InitializeProcThreadAttributeList(attributes, 1, 0, ref attributesSize)) throw new Win32Exception();
                attributesReady = true;
                handleList = Marshal.AllocHGlobal(IntPtr.Size * 2);
                Marshal.WriteIntPtr(handleList, 0, input);
                Marshal.WriteIntPtr(handleList, IntPtr.Size, output);
                if (!Native.UpdateProcThreadAttribute(attributes, 0, new IntPtr(0x00020002), handleList, new IntPtr(IntPtr.Size * 2), IntPtr.Zero, IntPtr.Zero)) throw new Win32Exception();
                StartupInfoEx si = new StartupInfoEx();
                si.startupInfo.cb = Marshal.SizeOf(typeof(StartupInfoEx));
                si.startupInfo.flags = 0x100; si.startupInfo.stdInput = input; si.startupInfo.stdOutput = output; si.startupInfo.stdError = output;
                si.attributeList = attributes;
                StringBuilder command = new StringBuilder(Quote(exe));
                foreach (string arg in args) command.Append(' ').Append(Quote(arg));
                uint flags = 0x4 | 0x400 | 0x80000 | (hidden ? 0x08000000u : 0u);
                if (!Native.CreateProcess(exe, command, IntPtr.Zero, IntPtr.Zero, true, flags, environment, cwd, ref si, out pi)) throw new Win32Exception();
                started = true;
                if (!Native.AssignProcessToJobObject(handle, pi.process)) throw new Win32Exception();
                if (Native.ResumeThread(pi.thread) == 0xffffffff) throw new Win32Exception();
                resumed = true;
                OwnedProcess child = new OwnedProcess(); child.Handle = pi.process; child.Id = (int)pi.processId;
                return child;
            } catch (Exception error) {
                startupError = error;
                throw;
            } finally {
                string cleanupFailure = null;
                if (started && !resumed) {
                    bool terminated = Native.TerminateProcess(pi.process, 1);
                    int terminateError = terminated ? 0 : Marshal.GetLastWin32Error();
                    uint waitResult = Native.WaitForSingleObject(pi.process, 5000);
                    int waitError = waitResult == 0xffffffff ? Marshal.GetLastWin32Error() : 0;
                    if (waitResult != 0) cleanupFailure = String.Format("Could not confirm suspended child PID {0} exited; terminate error={1}, wait result={2}, wait error={3}", pi.processId, terminateError, waitResult, waitError);
                    Native.CloseHandle(pi.process);
                }
                if (pi.thread != IntPtr.Zero) Native.CloseHandle(pi.thread);
                if (attributesReady) Native.DeleteProcThreadAttributeList(attributes);
                if (attributes != IntPtr.Zero) Marshal.FreeHGlobal(attributes);
                if (handleList != IntPtr.Zero) Marshal.FreeHGlobal(handleList);
                if (environment != IntPtr.Zero) Marshal.FreeHGlobal(environment);
                if (input != IntPtr.Zero && input != new IntPtr(-1)) Native.CloseHandle(input);
                Native.CloseHandle(output);
                if (cleanupFailure != null) throw new InvalidOperationException(cleanupFailure + "; startup error: " + (startupError == null ? "unknown" : startupError.Message), startupError);
            }
        }
        public void Dispose() { if (handle != IntPtr.Zero) { Native.CloseHandle(handle); handle = IntPtr.Zero; } }
    }
    static class Native {
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern IntPtr CreateJobObject(IntPtr attr, string name);
        [DllImport("kernel32.dll", SetLastError=true)] public static extern bool SetInformationJobObject(IntPtr job, int cls, IntPtr info, uint size);
        [DllImport("kernel32.dll", SetLastError=true)] public static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern IntPtr CreateFile(string name, uint access, uint share, ref SecurityAttributes attr, uint creation, uint flags, IntPtr template);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern bool CreateProcess(string app, StringBuilder command, IntPtr pa, IntPtr ta, bool inherit, uint flags, IntPtr env, string cwd, ref StartupInfoEx si, out ProcessInfo pi);
        [DllImport("kernel32.dll", SetLastError=true)] public static extern bool InitializeProcThreadAttributeList(IntPtr list, int count, int flags, ref IntPtr size);
        [DllImport("kernel32.dll", SetLastError=true)] public static extern bool UpdateProcThreadAttribute(IntPtr list, uint flags, IntPtr attribute, IntPtr value, IntPtr size, IntPtr previous, IntPtr returnSize);
        [DllImport("kernel32.dll")] public static extern void DeleteProcThreadAttributeList(IntPtr list);
        [DllImport("kernel32.dll", SetLastError=true)] public static extern uint ResumeThread(IntPtr thread);
        [DllImport("kernel32.dll", SetLastError=true)] public static extern bool TerminateProcess(IntPtr process, uint code);
        [DllImport("kernel32.dll", SetLastError=true)] public static extern bool GetExitCodeProcess(IntPtr process, out uint code);
        [DllImport("kernel32.dll", SetLastError=true)] public static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
        [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr handle);
    }
}
'@
}

function Start-OwnedFixture {
    param($Job, [string]$Exe, [string]$Root, $Environment, [bool]$NoSecureField, [string]$Baseline)
    $args = if ($NoSecureField) { @('--no-secure-field') } else { @('--message', $Baseline) }
    $log = Join-Path $Root ("fixture-" + [guid]::NewGuid().ToString('N') + '.log')
    $child = $Job.Start($Exe, [string[]]$args, $Root, $log, $false, $Environment)
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        do {
            if ($child.Wait(0)) { throw '专用 fixture 提前退出。' }
            $process = [Diagnostics.Process]::GetProcessById($child.Id)
            try {
                $process.Refresh()
                if ($process.MainWindowHandle -ne [IntPtr]::Zero -and $process.MainWindowTitle -eq 'Cortex Computer Use Fixture') { return $child }
            }
            finally { $process.Dispose() }
            Start-Sleep -Milliseconds 200
        } while ([DateTime]::UtcNow -lt $deadline)
        throw '专用 fixture 未在期限内创建预期窗口。'
    }
    catch { try { $child.Stop() } finally { $child.Dispose() }; throw }
}

function Invoke-OneTest {
    param($Job, [string]$Exe, [string]$Root, $Environment, [string]$Name, [int]$TimeoutSeconds, $Record)
    $Record.status = 'running'
    $Record['startedAt'] = [DateTime]::UtcNow.ToString('o')
    $log = Join-Path $Root ("test-" + [guid]::NewGuid().ToString('N') + '.log')
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $child = $null
    try {
        $child = $Job.Start($Exe, [string[]]@($Name, '--exact', '--ignored', '--test-threads=1'), $Root, $log, $true, $Environment)
        if (-not $child.Wait($TimeoutSeconds * 1000)) {
            $child.Stop()
            throw "测试超过 $TimeoutSeconds 秒：$Name"
        }
        $Record['exitCode'] = $child.ExitCode
        $Record['harness'] = Get-HarnessCheck -Text ([IO.File]::ReadAllText($log)) -Name $Name -ExitCode $child.ExitCode
        if (-not $Record.harness.passed) {
            throw "测试程序未运行并通过恰好一个指定测试：$Name"
        }
        $Record.status = 'passed'
    }
    catch { $Record.status = 'failed'; throw }
    finally {
        $watch.Stop()
        $Record['durationSeconds'] = [math]::Round($watch.Elapsed.TotalSeconds, 3)
        $Record['durationMilliseconds'] = $watch.ElapsedMilliseconds
        $Record['finishedAt'] = [DateTime]::UtcNow.ToString('o')
        if ($null -ne $child) { $child.Dispose() }
    }
}

$startedAt = [DateTime]::UtcNow
$locks = New-Object 'System.Collections.Generic.List[System.IO.FileStream]'
$job = $null
$fixture = $null
$tempRoot = $null
$tempRootCreated = $false
$resultStream = $null
$failure = $null
$cleanupErrors = New-Object 'System.Collections.Generic.List[string]'
$osInfo = [ordered]@{
    versionString = [Environment]::OSVersion.VersionString
    productName = $null
    displayVersion = $null
    buildNumber = $null
    updateBuildRevision = $null
}
try {
    $registryView = if ([Environment]::Is64BitOperatingSystem) { [Microsoft.Win32.RegistryView]::Registry64 } else { [Microsoft.Win32.RegistryView]::Registry32 }
    $registry = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $registryView)
    try {
        $osKey = $registry.OpenSubKey('SOFTWARE\Microsoft\Windows NT\CurrentVersion')
        if ($null -ne $osKey) {
            try {
                $osInfo.productName = $osKey.GetValue('ProductName')
                $osInfo.displayVersion = $osKey.GetValue('DisplayVersion')
                $osInfo.buildNumber = $osKey.GetValue('CurrentBuildNumber')
                $osInfo.updateBuildRevision = $osKey.GetValue('UBR')
            }
            finally { $osKey.Dispose() }
        }
    }
    finally { $registry.Dispose() }
}
catch { } # 版本注册表不可读时仍保留 Environment 的版本字符串。
$osArchitecture = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITEW6432')
if ([string]::IsNullOrWhiteSpace($osArchitecture)) { $osArchitecture = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITECTURE') }
if ([string]::IsNullOrWhiteSpace($osArchitecture)) { $osArchitecture = 'unknown' }
$result = [ordered]@{
    schemaVersion = 1
    suite = 'cortex-computer-use-windows-portable-second-host-smoke'
    status = 'failed'
    startedAt = $startedAt.ToString('o')
    finishedAt = $null
    machine = [Environment]::MachineName
    os = $osInfo
    osArchitecture = $osArchitecture
    powershellVersion = $PSVersionTable.PSVersion.ToString()
    requestedSeconds = $Seconds
    intervalMs = $IntervalMs
    runtimeOwnership = 'agent'
    inputs = [ordered]@{ test = $null; fixture = $null; vcRuntime = $null; cua = [ordered]@{} }
    tests = [ordered]@{
        secure = [ordered]@{ name = $secureName; status = 'not_run' }
        soak = [ordered]@{ name = $soakName; status = 'not_run' }
    }
}

try {
    if ($PSVersionTable.PSVersion.Major -ne 5 -or $PSVersionTable.PSVersion.Minor -ne 1) {
        throw '请使用 Windows PowerShell 5.1 运行此脚本。'
    }
    if (-not [Environment]::Is64BitOperatingSystem) { throw 'Cua 0.38 发布文件要求 64 位 Windows。' }
    $tokens = $null
    $parseErrors = $null
    $null = [Management.Automation.Language.Parser]::ParseFile($PSCommandPath, [ref]$tokens, [ref]$parseErrors)
    if (@($parseErrors).Count -ne 0) { throw "脚本语法错误：$($parseErrors[0].Message)" }
    $scriptBytes = [IO.File]::ReadAllBytes($PSCommandPath)
    if ($scriptBytes.Length -lt 3 -or $scriptBytes[0] -ne 0xef -or $scriptBytes[1] -ne 0xbb -or $scriptBytes[2] -ne 0xbf) {
        throw '脚本必须保存为带 BOM 的 UTF-8，供 Windows PowerShell 5.1 读取。'
    }
    Test-HarnessParser
    $testPath = Resolve-LocalPath -Value $TestExePath -MustExist $true -Directory $false
    $fixturePath = Resolve-LocalPath -Value $FixtureExePath -MustExist $true -Directory $false
    $runtimePath = Resolve-LocalPath -Value $VCRuntimePath -MustExist $true -Directory $false
    $cuaPath = Resolve-LocalPath -Value $CuaDirectory -MustExist $true -Directory $true
    $output = Resolve-LocalPath -Value $OutputPath -MustExist $false -Directory $false
    if ([IO.Path]::GetExtension($testPath) -ine '.exe' -or [IO.Path]::GetExtension($fixturePath) -ine '.exe') { throw '测试程序与 fixture 必须是 exe。' }
    if ([IO.Path]::GetFileName($runtimePath) -ine 'vcruntime140.dll') { throw 'VC 运行库文件名必须是 vcruntime140.dll。' }
    if ($fixturePath.IndexOfAny([char[]]";`r`n") -ge 0) { throw 'fixture 路径不能包含 allowlist 分隔符。' }
    if ([IO.Path]::GetExtension($output) -ine '.json') { throw '结果路径必须使用 .json 扩展名。' }
    if (-not (Test-Path -LiteralPath ([IO.Path]::GetDirectoryName($output)) -PathType Container)) { throw '结果目录不存在。' }
    if (Test-Path -LiteralPath $output) { throw '结果路径已存在；为避免覆盖，请指定新文件。' }
    if (-not $ValidateOnly) {
        $resultStream = [IO.File]::Open($output, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    }
    $result.inputs.test = [ordered]@{ path = $testPath; expectedSha256 = $ExpectedTestSha256.ToLowerInvariant(); sha256 = $null }
    $result.inputs.fixture = [ordered]@{ path = $fixturePath; expectedSha256 = $ExpectedFixtureSha256.ToLowerInvariant(); sha256 = $null }
    $result.inputs.vcRuntime = [ordered]@{ path = $runtimePath; expectedSha256 = $ExpectedVCRuntimeSha256.ToLowerInvariant(); sha256 = $null }
    $result.inputs.test.sha256 = Open-VerifiedInput -Path $testPath -Expected $ExpectedTestSha256 -Locks $locks
    $result.inputs.fixture.sha256 = Open-VerifiedInput -Path $fixturePath -Expected $ExpectedFixtureSha256 -Locks $locks
    $result.inputs.vcRuntime.sha256 = Open-VerifiedInput -Path $runtimePath -Expected $ExpectedVCRuntimeSha256 -Locks $locks
    foreach ($name in $cuaHashes.Keys) {
        $path = Resolve-LocalPath -Value (Join-Path $cuaPath $name) -MustExist $true -Directory $false
        $result.inputs.cua[$name] = [ordered]@{ path = $path; sha256 = (Open-VerifiedInput -Path $path -Expected $cuaHashes[$name] -Locks $locks) }
    }
    if ($ValidateOnly) {
        Write-Output '校验通过：路径、全部输入哈希、PowerShell 5.1 语法、UTF-8 BOM 及单测试结果解析器；未启动任何进程，未创建 JSON。测试是否存在须由实际运行确认。'
        return
    }

    $tempRoot = Join-Path ([IO.Path]::GetTempPath()) ('cortex-second-host-' + [guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path $tempRoot
    $tempRootCreated = $true
    foreach ($directory in @('tests', 'fixture', 'cua', 'driver-home', 'profile', 'appdata', 'localappdata', 'tmp')) {
        $null = New-Item -ItemType Directory -Path (Join-Path $tempRoot $directory)
    }
    # 独立目录避免从输入程序所在目录加载额外 DLL，也避免命中已有 fixture 窗口。
    $isolatedTestPath = Join-Path (Join-Path $tempRoot 'tests') 'cortex-local-tests.exe'
    [IO.File]::Copy($testPath, $isolatedTestPath, $false)
    $isolatedTestHash = Open-VerifiedInput -Path $isolatedTestPath -Expected $result.inputs.test.sha256 -Locks $locks
    $result.inputs.test['executionPath'] = $isolatedTestPath
    $result.inputs.test['executionSha256'] = $isolatedTestHash
    # Rust 测试程序动态链接 VC runtime；只从独立目录加载已校验的运行库。
    $isolatedRuntimePath = Join-Path (Join-Path $tempRoot 'tests') 'vcruntime140.dll'
    [IO.File]::Copy($runtimePath, $isolatedRuntimePath, $false)
    $result.inputs.vcRuntime['executionPath'] = $isolatedRuntimePath
    $result.inputs.vcRuntime['executionSha256'] = Open-VerifiedInput -Path $isolatedRuntimePath -Expected $result.inputs.vcRuntime.sha256 -Locks $locks
    $isolatedFixturePath = Join-Path (Join-Path $tempRoot 'fixture') 'cortex-computer-use-soak-fixture.exe'
    if ($isolatedFixturePath.IndexOfAny([char[]]";`r`n") -ge 0) { throw '临时 fixture 路径不能包含 allowlist 分隔符。' }
    [IO.File]::Copy($fixturePath, $isolatedFixturePath, $false)
    $isolatedHash = Open-VerifiedInput -Path $isolatedFixturePath -Expected $result.inputs.fixture.sha256 -Locks $locks
    $result.inputs.fixture['executionPath'] = $isolatedFixturePath
    $result.inputs.fixture['executionSha256'] = $isolatedHash
    $isolatedCuaPath = Join-Path $tempRoot 'cua'
    foreach ($name in $cuaHashes.Keys) {
        $isolatedFile = Join-Path $isolatedCuaPath $name
        [IO.File]::Copy($result.inputs.cua[$name].path, $isolatedFile, $false)
        $copiedHash = Open-VerifiedInput -Path $isolatedFile -Expected $cuaHashes[$name] -Locks $locks
        $result.inputs.cua[$name]['executionPath'] = $isolatedFile
        $result.inputs.cua[$name]['executionSha256'] = $copiedHash
    }
    $manifest = Join-Path $tempRoot 'manifest.json'
    $pipe = '\\.\pipe\cortex-second-host-' + [guid]::NewGuid().ToString('N')
    $manifestContent = [ordered]@{
        version = 3; expires_after = '8h'; idle_timeout = '30m'
        resources = @{ apps = @(@{ executable = $isolatedFixturePath; windows = 'all'; terminate = 'deny' }) }
        allow = @{ tools = @('start_session', 'get_window_state', 'click', 'type_text', 'set_value', 'press_key', 'scroll', 'end_session') }
    } | ConvertTo-Json -Depth 8
    # Rust 侧以 UTF-8 文本读取 manifest；不要写入 Windows PowerShell 的 BOM。
    [IO.File]::WriteAllText($manifest, $manifestContent, (New-Object Text.UTF8Encoding $false))
    $manifestHash = Get-CapabilityManifestHash -Bytes ([IO.File]::ReadAllBytes($manifest))
    $result['capabilityManifestSha256'] = $manifestHash
    $driverPath = Join-Path $isolatedCuaPath 'cua-driver.exe'
    $envMap = New-Object 'System.Collections.Generic.Dictionary[string,string]' ([StringComparer]::OrdinalIgnoreCase)
    $envMap['SystemRoot'] = $env:SystemRoot
    $envMap['WINDIR'] = $env:SystemRoot
    $envMap['PATH'] = Get-PortableSystemPath
    $envMap['TEMP'] = Join-Path $tempRoot 'tmp'
    $envMap['TMP'] = $envMap['TEMP']
    $envMap['USERPROFILE'] = Join-Path $tempRoot 'profile'
    $envMap['APPDATA'] = Join-Path $tempRoot 'appdata'
    $envMap['LOCALAPPDATA'] = Join-Path $tempRoot 'localappdata'
    $envMap['CUA_DRIVER_RS_HOME'] = Join-Path $tempRoot 'driver-home'
    $envMap['CUA_DRIVER_RS_TELEMETRY_ENABLED'] = 'false'
    $envMap['CUA_DRIVER_RS_UPDATE_CHECK'] = 'false'
    $envMap['CUA_TELEMETRY_ENABLED'] = 'false'
    $envMap['CUA_TELEMETRY'] = '0'
    $envMap['CUA_TELEMETRY_FORBID_NETWORK'] = '1'
    $envMap['DO_NOT_TRACK'] = '1'
    $envMap['CORTEX_CUA_DRIVER_PATH'] = $driverPath
    $envMap['CORTEX_CUA_RUNTIME_OWNER'] = 'agent'
    $envMap['CORTEX_CUA_DRIVER_SOCKET'] = $pipe
    $envMap['CORTEX_CUA_EXECUTABLE_ALLOWLIST'] = $isolatedFixturePath
    $envMap['CORTEX_CUA_ALLOW_ACTIONS'] = '1'
    $envMap['CORTEX_CUA_MANIFEST_PATH'] = $manifest
    $envMap['CORTEX_CUA_DRIVER_SHA256'] = $cuaHashes['cua-driver.exe']
    $envMap['CORTEX_CUA_SESSION'] = 'second-host-' + [guid]::NewGuid().ToString('N')
    $envMap['CORTEX_CUA_SOAK_SECONDS'] = "$Seconds"
    $envMap['CORTEX_CUA_SOAK_INTERVAL_MS'] = "$IntervalMs"
    $envMap['CORTEX_COMPUTER_TRACE_DIR'] = Join-Path $tempRoot 'traces'
    $envMap['TEST_CORTEX_FIXTURE_SHA256'] = $isolatedHash
    $envMap['TEST_CORTEX_FIXTURE_BASELINE'] = $secureBaseline
    $envMap['RUST_LOG'] = 'cortex_local=info'

    Initialize-OwnedProcessSupport
    $job = New-Object CortexPortableSmoke.OwnedJob
    # 原生 sidecar 由 Rust 的 owned runtime 启动与清退，不能用父脚本的
    # external daemon 让原 Job custody 和可信派发接线缺失的测试看起来通过。
    $envMap['TEST_CORTEX_PORTABLE_ATTACH_FIXTURE'] = '1'
    $fixture = Start-OwnedFixture -Job $job -Exe $isolatedFixturePath -Root $tempRoot -Environment $envMap -NoSecureField $false -Baseline $secureBaseline
    $envMap['TEST_CORTEX_FIXTURE_PID'] = [string]$fixture.Id
    Invoke-OneTest -Job $job -Exe $isolatedTestPath -Root $tempRoot -Environment $envMap -Name $secureName -TimeoutSeconds 180 -Record $result.tests.secure
    $fixture.Stop()
    $fixture.Dispose()
    $fixture = $null
    $fixture = Start-OwnedFixture -Job $job -Exe $isolatedFixturePath -Root $tempRoot -Environment $envMap -NoSecureField $true -Baseline $secureBaseline
    $envMap['TEST_CORTEX_FIXTURE_PID'] = [string]$fixture.Id
    Invoke-OneTest -Job $job -Exe $isolatedTestPath -Root $tempRoot -Environment $envMap -Name $soakName -TimeoutSeconds ($Seconds + 300) -Record $result.tests.soak
    if ($result.tests.soak.durationMilliseconds -lt ([long]$Seconds * 1000)) {
        $result.tests.soak.status = 'failed'
        throw "普通 soak 实际运行时间短于要求的 $Seconds 秒。"
    }
    $result.status = 'passed'
}
catch {
    $failure = $_.Exception.Message
    if ($ValidateOnly) { throw }
}
finally {
    foreach ($owned in @($fixture)) {
        if ($null -ne $owned) {
            try { $owned.Stop() } catch { $cleanupErrors.Add("清理进程失败：$($_.Exception.Message)") }
            finally { $owned.Dispose() }
        }
    }
    if ($null -ne $job) { $job.Dispose() }
    foreach ($stream in $locks) { $stream.Dispose() }
    if ($tempRootCreated -and (Test-Path -LiteralPath $tempRoot -PathType Container)) {
        if ($null -ne $failure) {
            try {
                $logs = @(Get-ChildItem -LiteralPath $tempRoot -File)
                $selected = @($logs | Where-Object { $_.Name -eq 'daemon.log' -or $_.Name -match '^test-[0-9a-f]+\.log$' })
                $selected += @($logs | Where-Object { $_.Name -match '^status-[0-9]+\.log$' } | Sort-Object LastWriteTime -Descending | Select-Object -First 4)
                $evidence = @($selected | ForEach-Object { Get-SafeLogEvidence -Path $_.FullName })
                $result['diagnostics'] = $evidence
            }
            catch { $result['diagnosticsError'] = '无法读取受限的诊断摘要。' }
        }
        # 关闭 job 会结束子进程树；给刚退出的 helper 一点时间释放文件句柄。
        for ($attempt = 0; $attempt -lt 20; $attempt++) {
            try { [IO.Directory]::Delete($tempRoot, $true); break }
            catch {
                if ($attempt -eq 19) { $cleanupErrors.Add("清理临时目录失败：$($_.Exception.Message)") }
                else { Start-Sleep -Milliseconds 250 }
            }
        }
    }
    if ($cleanupErrors.Count -gt 0) {
        $result['cleanupErrors'] = @($cleanupErrors.ToArray())
        $result.status = 'failed'
        if ($null -eq $failure) { $failure = $cleanupErrors[0] }
    }
    if ($null -ne $resultStream) {
        $result.finishedAt = [DateTime]::UtcNow.ToString('o')
        $result['durationSeconds'] = [math]::Round(([DateTime]::UtcNow - $startedAt).TotalSeconds, 3)
        if ($null -ne $failure) { $result['failure'] = $failure }
        try {
            $json = $result | ConvertTo-Json -Depth 12
            $bytes = (New-Object Text.UTF8Encoding $true).GetPreamble() + [Text.Encoding]::UTF8.GetBytes($json)
            $resultStream.Write($bytes, 0, $bytes.Length)
        }
        finally { $resultStream.Dispose() }
    }
}
if ($null -ne $failure) { throw $failure }
Write-Output "结果已保存：$output"
