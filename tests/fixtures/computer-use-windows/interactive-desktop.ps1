# 仅声明函数；导入不查询桌面、不启动进程。单文件可随第二台 Windows 验收包复制。
function Initialize-InteractiveDesktopNative {
    if ('CortexInteractiveDesktop.Windows' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace CortexInteractiveDesktop {
    public sealed class SessionInformation {
        public uint Level, SessionId, Flags;
        public int State;
    }
    public sealed class ForegroundInformation {
        public bool HasForeground;
        public uint SessionId;
        public string ProcessName;
    }
    public sealed class Evidence {
        public bool Allowed { get; internal set; }
        public string Reason { get; internal set; }
        public uint? ProcessSessionId { get; internal set; }
        public uint? SessionLevel { get; internal set; }
        public uint? ReportedSessionId { get; internal set; }
        public int? SessionState { get; internal set; }
        public uint? SessionFlags { get; internal set; }
        public string WindowStation { get; internal set; }
        public string InputDesktop { get; internal set; }
        public bool? HasForeground { get; internal set; }
        public bool? ForegroundIdentityVerified { get; internal set; }
        public string ErrorType { get; internal set; }
        public int? Win32Error { get; internal set; }
    }
    public static class Windows {
        // 同源于 readonly-ab/cua-readonly-ab.ps1，与产品 computer_desktop.rs 对齐。
        // 微软 WTSINFOEX_LEVEL1_W 的完整布局保留 8 字节对齐，不能把 Data 误放在偏移 4。
        // https://learn.microsoft.com/windows/win32/api/wtsapi32/ns-wtsapi32-wtsinfoex_level1_w
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
        [DllImport("kernel32.dll")] private static extern uint GetCurrentProcessId();
        [DllImport("kernel32.dll", SetLastError=true)]
        private static extern bool ProcessIdToSessionId(uint pid, out uint session);
        [DllImport("wtsapi32.dll", CharSet=CharSet.Unicode, ExactSpelling=true, SetLastError=true)]
        private static extern bool WTSQuerySessionInformationW(IntPtr server, uint session, int infoClass, out IntPtr buffer, out uint bytes);
        [DllImport("wtsapi32.dll")] private static extern void WTSFreeMemory(IntPtr buffer);
        [DllImport("user32.dll", SetLastError=true)] private static extern IntPtr GetProcessWindowStation();
        [DllImport("user32.dll", SetLastError=true)] private static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
        [DllImport("user32.dll", SetLastError=true)] private static extern bool CloseDesktop(IntPtr desktop);
        [DllImport("user32.dll", CharSet=CharSet.Unicode, ExactSpelling=true, SetLastError=true)]
        private static extern bool GetUserObjectInformationW(IntPtr handle, int index, IntPtr buffer, uint length, out uint needed);
        [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll", SetLastError=true)]
        private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);

        public static bool SessionIdentityAllowed(uint processSession, uint level, uint reportedSession) {
            return processSession != 0 && level == 1 && reportedSession == processSession;
        }
        private static string DesktopEvidenceFailure(int state, uint flags, string station, string desktop,
                bool hasForeground, string foregroundProcess) {
            // WTSActive=0，WTS_SESSIONSTATE_UNLOCK=1；前台窗口为空并不是锁屏证据。
            if (state != 0) return "SESSION_NOT_ACTIVE";
            if (flags != 1) return "SESSION_NOT_UNLOCKED";
            if (!String.Equals(station, "WinSta0", StringComparison.OrdinalIgnoreCase)) return "WINDOW_STATION_NOT_INTERACTIVE";
            if (!String.Equals(desktop, "Default", StringComparison.OrdinalIgnoreCase)) return "INPUT_DESKTOP_NOT_DEFAULT";
            if (!hasForeground) return null;
            if (String.IsNullOrWhiteSpace(foregroundProcess)) return "FOREGROUND_IDENTITY_UNKNOWN";
            foreach (string secureProcess in new[] { "LockApp", "LogonUI", "Winlogon" }) {
                if (String.Equals(foregroundProcess, secureProcess, StringComparison.OrdinalIgnoreCase))
                    return "FOREGROUND_SECURE_PROCESS";
            }
            return null;
        }
        public static bool DesktopEvidenceAllowed(int state, uint flags, string station, string desktop,
                bool hasForeground, string foregroundProcess) {
            return DesktopEvidenceFailure(state, flags, station, desktop, hasForeground, foregroundProcess) == null;
        }

        public static Evidence InteractiveDesktop() {
            return InteractiveDesktop(CurrentSession, QuerySession, WindowStationName, InputDesktopName, Foreground);
        }
        // 把证据读取与判定分开，回归可注入查询失败和零 HWND，不必操纵真实桌面。
        public static Evidence InteractiveDesktop(Func<uint> currentSession,
                Func<uint, SessionInformation> querySession, Func<string> windowStation,
                Func<string> inputDesktop, Func<ForegroundInformation> foreground) {
            var result = new Evidence();
            string stage = "PROCESS_SESSION_QUERY_FAILED";
            try {
                uint session = currentSession();
                result.ProcessSessionId = session;
                if (session == 0) { result.Reason = "SESSION_ZERO"; return result; }
                stage = "SESSION_INFORMATION_QUERY_FAILED";
                var info = querySession(session);
                if (info == null) throw new InvalidDataException();
                result.SessionLevel = info.Level;
                result.ReportedSessionId = info.SessionId;
                result.SessionState = info.State;
                result.SessionFlags = info.Flags;
                if (!SessionIdentityAllowed(session, info.Level, info.SessionId)) {
                    result.Reason = info.Level != 1 ? "SESSION_LEVEL_UNKNOWN" : "SESSION_ID_MISMATCH";
                    return result;
                }
                // 先排除锁屏/断连，再查桌面；OpenInputDesktop 在断连时也可能返回句柄。
                if (info.State != 0 || info.Flags != 1) {
                    result.Reason = DesktopEvidenceFailure(info.State, info.Flags, null, null, false, null);
                    return result;
                }
                stage = "WINDOW_STATION_QUERY_FAILED";
                result.WindowStation = windowStation();
                stage = "INPUT_DESKTOP_QUERY_FAILED";
                result.InputDesktop = inputDesktop();
                result.Reason = DesktopEvidenceFailure(info.State, info.Flags,
                    result.WindowStation, result.InputDesktop, false, null);
                if (result.Reason != null) return result;
                stage = "FOREGROUND_IDENTITY_QUERY_FAILED";
                var front = foreground();
                if (front == null) throw new InvalidDataException();
                result.HasForeground = front.HasForeground;
                if (front.HasForeground) {
                    result.ForegroundIdentityVerified = false;
                    if (front.SessionId != session) { result.Reason = "FOREGROUND_SESSION_MISMATCH"; return result; }
                }
                result.Reason = DesktopEvidenceFailure(info.State, info.Flags,
                    result.WindowStation, result.InputDesktop, front.HasForeground, front.ProcessName);
                if (result.Reason != null) return result;
                if (front.HasForeground) result.ForegroundIdentityVerified = true;
                result.Allowed = true;
                result.Reason = "INTERACTIVE_DESKTOP_ALLOWED";
            }
            catch (Exception exception) {
                result.Reason = stage;
                var error = exception.GetBaseException();
                result.ErrorType = error.GetType().Name;
                var win32 = error as Win32Exception;
                if (win32 != null) result.Win32Error = win32.NativeErrorCode;
            }
            return result;
        }

        private static uint CurrentSession() {
            // Win7 的锁屏标记有已知反转；本模块只接受产品支持的 Win10/11 语义。
            if (Environment.OSVersion.Platform != PlatformID.Win32NT || Environment.OSVersion.Version.Major < 10)
                throw new PlatformNotSupportedException();
            uint session;
            if (!ProcessIdToSessionId(GetCurrentProcessId(), out session)) throw new Win32Exception(Marshal.GetLastWin32Error());
            return session;
        }
        private static SessionInformation ReadSessionInformation(IntPtr buffer, uint bytes) {
            if (buffer == IntPtr.Zero || bytes < Marshal.SizeOf(typeof(SessionInfoEx))) throw new InvalidDataException();
            var info = (SessionInfoEx)Marshal.PtrToStructure(buffer, typeof(SessionInfoEx));
            return new SessionInformation { Level = info.Level, SessionId = info.Data.SessionId,
                State = info.Data.SessionState, Flags = unchecked((uint)info.Data.SessionFlags) };
        }
        private static SessionInformation QuerySession(uint session) {
            IntPtr buffer = IntPtr.Zero;
            try {
                uint bytes;
                if (!WTSQuerySessionInformationW(IntPtr.Zero, session, 25, out buffer, out bytes))
                    throw new Win32Exception(Marshal.GetLastWin32Error()); // WTSSessionInfoEx=25
                return ReadSessionInformation(buffer, bytes);
            }
            finally { if (buffer != IntPtr.Zero) WTSFreeMemory(buffer); }
        }
        private static string ObjectName(IntPtr handle) {
            if (handle == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
            uint needed;
            GetUserObjectInformationW(handle, 2, IntPtr.Zero, 0, out needed); // UOI_NAME=2
            if (needed < 2 || needed > 1024 || (needed & 1) != 0) throw new InvalidDataException();
            uint capacity = needed;
            IntPtr buffer = Marshal.AllocHGlobal((int)capacity);
            try {
                if (!GetUserObjectInformationW(handle, 2, buffer, capacity, out needed))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                if (needed < 2 || needed > capacity || (needed & 1) != 0) throw new InvalidDataException();
                var bytes = new byte[needed];
                Marshal.Copy(buffer, bytes, 0, bytes.Length);
                int end = 0;
                while (end < bytes.Length && (bytes[end] != 0 || bytes[end + 1] != 0)) end += 2;
                if (end == bytes.Length) throw new InvalidDataException();
                // 无效 UTF-16 必须拒绝，不能静默替换后再比较身份。
                return new UnicodeEncoding(false, false, true).GetString(bytes, 0, end);
            }
            finally { Marshal.FreeHGlobal(buffer); }
        }
        private static string WindowStationName() { return ObjectName(GetProcessWindowStation()); }
        private static string InputDesktopName() {
            IntPtr desktop = OpenInputDesktop(0, false, 1); // DESKTOP_READOBJECTS；不切换、不绑定桌面。
            if (desktop == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
            try { return ObjectName(desktop); }
            finally { if (!CloseDesktop(desktop)) throw new Win32Exception(Marshal.GetLastWin32Error()); }
        }
        private static ForegroundInformation Foreground() {
            IntPtr window = GetForegroundWindow();
            if (window == IntPtr.Zero) return new ForegroundInformation { HasForeground = false };
            uint pid;
            if (GetWindowThreadProcessId(window, out pid) == 0 || pid == 0)
                throw new Win32Exception(Marshal.GetLastWin32Error());
            // 只核对已返回的前台进程，不枚举窗口、进程或读取窗口内容。
            using (var process = Process.GetProcessById(checked((int)pid))) {
                return new ForegroundInformation { HasForeground = true, SessionId = checked((uint)process.SessionId),
                    ProcessName = process.ProcessName };
            }
        }
    }
}
'@
}

function Get-InteractiveDesktopEvidence {
    Initialize-InteractiveDesktopNative
    return [CortexInteractiveDesktop.Windows]::InteractiveDesktop()
}

function Assert-InteractiveDesktop {
    $evidence = Get-InteractiveDesktopEvidence
    if (-not $evidence.Allowed) {
        # 查询失败只说明证据不足，不能声称 Windows 一定处于锁屏状态。
        throw "无法确认当前会话是已解锁的 WinSta0/Default 交互桌面，拒绝继续验收。证据=$($evidence.Reason)；查询错误=$($evidence.ErrorType)；Win32=$($evidence.Win32Error)"
    }
}
