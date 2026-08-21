import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

import 'local_agent_io.dart' show stateDir;

/// 锁的持有者。**必须活到进程退出** —— 存局部变量的话 GC 一收，
/// 文件句柄关掉，锁就没了，而界面还开着。
RandomAccessFile? _lockHolder;

/// 只给测试用：放掉锁，好让同一个测试进程模拟「第二份实例」。
/// 生产代码**永远不调它** —— 锁的全部意义就是与进程同生共死。
void releaseSingleInstanceLockForTest() {
  _lockHolder?.closeSync();
  _lockHolder = null;
}

/// 争这台机器上「唯一一份桌面端」的名额。
///
/// 返回 true = 名额是我们的，继续启动。
/// 返回 false = 已经有一份在跑（已尽力把它的窗口带到前台），调用方应当
/// **立刻退出，什么都别碰** —— 走到这里时凭据库、settings.json、agent
/// 一样都还没动过，这正是这个检查必须排在 `main()` 第一行的原因。
///
/// 为什么是排它锁而不是 PID 文件、以及它防的是哪次事故，见
/// `single_instance.dart` 的库注释。
bool ensureSingleInstance() {
  final dir = stateDir();
  // 连状态目录都解析不出来（缺 %LOCALAPPDATA%）的环境里没有可共享的
  // 状态，也就没有要防的冲突 —— 放行，别把启动挡死在一个防护措施上
  if (dir == null) return true;

  try {
    Directory(dir).createSync(recursive: true);
    final raf = File(
      '$dir${Platform.pathSeparator}app.lock',
    ).openSync(mode: FileMode.append);
    // 排它锁整个文件。另一份进程持着锁时这里抛 FileSystemException ——
    // 那就是「已经有一份在跑」的判据，由内核担保，没有陈旧问题
    raf.lockSync(FileLock.exclusive);
    _lockHolder = raf;
    return true;
  } on FileSystemException {
    _focusExisting();
    return false;
  } on Object {
    // 锁不上但原因不是「被人持着」（权限、磁盘只读……）：放行。
    // 防护措施自己出故障时宁可少防一层，也不能把应用挡在门外 ——
    // 与启动诊断日志「写失败一律吞掉」是同一个立场
    return true;
  }
}

/// 把已经在跑的那份的窗口带到前台。
///
/// 尽力而为：找不到、置不了前台都**静默放弃** —— 这时用户点了图标没反应，
/// 不理想，但比弹一个「已在运行」的对话框再让他自己去任务栏里翻要好不了
/// 多少，而比两份进程互相腐蚀共享状态好得多。
void _focusExisting() {
  if (!Platform.isWindows) return;
  try {
    final user32 = DynamicLibrary.open('user32.dll');
    final findWindow = user32
        .lookupFunction<
          IntPtr Function(Pointer<Utf16>, Pointer<Utf16>),
          int Function(Pointer<Utf16>, Pointer<Utf16>)
        >('FindWindowW');
    final showWindow = user32
        .lookupFunction<Int32 Function(IntPtr, Int32), int Function(int, int)>(
          'ShowWindow',
        );
    final setForeground = user32
        .lookupFunction<Int32 Function(IntPtr), int Function(int)>(
          'SetForegroundWindow',
        );

    // 按标题找。此刻**我们自己的窗口还不存在**（检查在 runApp 之前），
    // 所以找到的必然是先来那份的。两个构建标题都叫 Cortex —— 这里
    // 恰好是「分不清是哪份」帮了忙：不管找到哪份，它都是该被聚焦的那份
    final title = 'Cortex'.toNativeUtf16();
    try {
      final hwnd = findWindow(nullptr, title);
      if (hwnd == 0) return;
      const swRestore = 9; // 最小化的要先还原，直接置前台是无效的
      showWindow(hwnd, swRestore);
      // 我们此刻是前台进程（用户刚双击了图标），把前台**让**出去是
      // Windows 允许的方向 —— 拦的是后台进程抢前台，不拦这个
      setForeground(hwnd);
    } finally {
      calloc.free(title);
    }
  } on Object {
    // 聚焦失败不值得让退出流程炸掉
  }
}
