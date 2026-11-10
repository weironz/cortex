// 仅对已持有的私有 fixture HWND 做只读 UIA 校验；没有桌面遍历或写入入口。
using System;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using System.Windows.Automation;

namespace CortexCandidateActions {
    public sealed class Witness {
        public int Pid;
        public long WindowId;
        public string RootId, Framework, InputId, InputName, InputValue, ButtonId, ButtonName, StatusId, StatusName, StatusValue;
        public bool InputReadOnly, InputPassword, InputEnabled, ButtonEnabled, ButtonInvoke, StatusReadOnly;
        public int SecureCount, InputMatches, ButtonMatches;
    }
    public static class Native {
        [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
        [DllImport("kernel32.dll", SetLastError=true)] static extern bool IsProcessInJob(IntPtr process, IntPtr job, out bool result);
        public static bool InJob() {
            bool result;
            return IsProcessInJob(GetCurrentProcess(), IntPtr.Zero, out result) && result;
        }
        static AutomationElement Only(AutomationElement root, string id) {
            var found = root.FindAll(TreeScope.Descendants, new PropertyCondition(AutomationElement.AutomationIdProperty, id));
            if (found.Count != 1) throw new InvalidOperationException("CA_NATIVE_CONTROL_ID");
            return found[0];
        }
        static int Matches(AutomationElement root, ControlType role, string name) {
            return root.FindAll(TreeScope.Descendants, new AndCondition(
                new PropertyCondition(AutomationElement.ControlTypeProperty, role),
                new PropertyCondition(AutomationElement.NameProperty, name))).Count;
        }
        static Witness Read(long hwnd, int pid) {
            var root = AutomationElement.FromHandle(new IntPtr(hwnd));
            if (root == null || root.Current.ProcessId != pid || root.Current.Name != "Cortex Computer Use Fixture"
                    || root.Current.ControlType != ControlType.Window) throw new InvalidOperationException("CA_NATIVE_WINDOW");
            // 在读取任何文本值之前拒绝整个安全输入场景，绝不读取 PasswordBox。
            int secure = root.FindAll(TreeScope.Descendants, new PropertyCondition(AutomationElement.IsPasswordProperty, true)).Count;
            if (secure != 0 || root.FindAll(TreeScope.Descendants, new PropertyCondition(AutomationElement.AutomationIdProperty, "1003")).Count != 0)
                throw new InvalidOperationException("CA_NATIVE_SECURE_FIELD");
            var input = Only(root, "1001"); var button = Only(root, "1002"); var status = Only(root, "1004");
            if (input.Current.ProcessId != pid || button.Current.ProcessId != pid || status.Current.ProcessId != pid
                    || input.Current.ControlType != ControlType.Edit || status.Current.ControlType != ControlType.Edit
                    || button.Current.ControlType != ControlType.Button) throw new InvalidOperationException("CA_NATIVE_CONTROL_ROLE");
            var iv = input.GetCurrentPattern(ValuePattern.Pattern) as ValuePattern;
            var sv = status.GetCurrentPattern(ValuePattern.Pattern) as ValuePattern;
            object invoke;
            if (iv == null || sv == null) throw new InvalidOperationException("CA_NATIVE_VALUE_PATTERN");
            return new Witness {
                Pid=pid, WindowId=hwnd, RootId=root.Current.AutomationId, Framework=root.Current.FrameworkId,
                InputId=input.Current.AutomationId, InputName=input.Current.Name, InputValue=iv.Current.Value,
                InputReadOnly=iv.Current.IsReadOnly, InputPassword=input.Current.IsPassword, InputEnabled=input.Current.IsEnabled,
                ButtonId=button.Current.AutomationId, ButtonName=button.Current.Name, ButtonEnabled=button.Current.IsEnabled,
                ButtonInvoke=button.TryGetCurrentPattern(InvokePattern.Pattern, out invoke),
                StatusId=status.Current.AutomationId, StatusName=status.Current.Name, StatusValue=sv.Current.Value,
                StatusReadOnly=sv.Current.IsReadOnly, SecureCount=secure,
                InputMatches=Matches(root, ControlType.Edit, "Message input"), ButtonMatches=Matches(root, ControlType.Button, "Copy message")
            };
        }
        public static Witness ReadBounded(long hwnd, int pid, int timeoutMs) {
            if (timeoutMs < 1 || timeoutMs > 5000) throw new InvalidOperationException("CA_NATIVE_BUDGET");
            var task = Task.Run(() => Read(hwnd, pid));
            if (!task.Wait(timeoutMs)) throw new InvalidOperationException("CA_NATIVE_TIMEOUT");
            return task.GetAwaiter().GetResult();
        }
    }
}
