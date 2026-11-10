// 复用 portable-second-host-smoke.ps1 的原始 suspended/Job/句柄白名单启动实现。
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

namespace CortexReadonlyOwned {
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
        public uint ActiveCount {
            get {
                BasicAccounting info;
                if (!Native.QueryInformationJobObject(handle, 1, out info,
                    (uint)Marshal.SizeOf(typeof(BasicAccounting)), IntPtr.Zero)) throw new Win32Exception();
                return info.ActiveProcesses;
            }
        }
        // 关闭句柄触发 kill-on-close 不能单独当作排空证明；持原 Job 查询 active=0。
        public bool StopAndWait(int milliseconds) {
            if (!Native.TerminateJobObject(handle, 1)) throw new Win32Exception();
            var clock = System.Diagnostics.Stopwatch.StartNew();
            do {
                if (ActiveCount == 0) return true;
                System.Threading.Thread.Sleep(25);
            } while (clock.ElapsedMilliseconds < milliseconds);
            return false;
        }
        public void Dispose() { if (handle != IntPtr.Zero) { Native.CloseHandle(handle); handle = IntPtr.Zero; } }
    }
    [StructLayout(LayoutKind.Sequential)] struct BasicAccounting {
        public long TotalUserTime, TotalKernelTime, ThisPeriodUserTime, ThisPeriodKernelTime;
        public uint TotalPageFaultCount, TotalProcesses, ActiveProcesses, TotalTerminatedProcesses;
    }
    static class Native {
        [DllImport("kernel32.dll", SetLastError=true)] public static extern bool TerminateJobObject(IntPtr job, uint code);
        [DllImport("kernel32.dll", SetLastError=true)] public static extern bool QueryInformationJobObject(IntPtr job, int cls, out BasicAccounting info, uint size, IntPtr length);
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
