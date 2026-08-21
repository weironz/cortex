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

/// 抢不到名额时**真的把这个进程结束掉**。
///
/// # 为什么 `main()` 里 `return` 不够
///
/// 桌面端的窗口消息循环在 **C++ runner** 里（`windows/runner/main.cpp` →
/// `FlutterWindow`），不由 Dart 的 `main()` 驱动。原生窗口在 Dart 代码
/// 跑起来**之前**就已经建好并显示了，所以从 `main()` 里 return 只是
/// 「不 runApp」而已 —— 进程照样活着，屏幕上多一扇**永远白屏的 Cortex
/// 窗口**，任务栏里两个一模一样的图标。
///
/// 2026-08-21 实测到的就是这个：单实例锁确实拦住了第二份去碰凭据、
/// settings.json 与 agent（要防的数据损坏确实防住了），但留下一个幽灵窗口，
/// 而 `just app-status` 会如实报「跑了不止一个」—— 一个由防护措施自己
/// 造出来的告警。
void exitDuplicateInstance() => exit(0);

/// 争这台机器上「唯一一份桌面端」的名额。
///
/// 返回 true = 名额是我们的，继续启动。
/// 返回 false = 已经有一份在跑（已尽力把它的窗口带到前台），调用方应当
/// **立刻退出，什么都别碰** —— 走到这里时凭据库、settings.json、agent
/// 一样都还没动过，这正是这个检查必须排在 `main()` 第一行的原因。
///
/// 为什么是排它锁而不是 PID 文件、以及它防的是哪次事故，见
/// `single_instance.dart` 的库注释。
///
/// [dirOverride] **只给测试用**。不给的话锁落在真实状态目录里 ——
/// 而测试若也去锁那一个，它就变成了「此刻这台机器上有没有跑着 Cortex」
/// 的探测：开发者一边开着应用一边 `just ci`，测试必红，而被测的机制
/// 完全正常。测试该测机制，不该测机器状态。
bool ensureSingleInstance({String? dirOverride}) {
  final dir = dirOverride ?? stateDir();
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
    // `FindWindowExW` 而不是 `FindWindowW`：后者只给第一个匹配，
    // 而我们需要**跳过自己那扇**（见下面那段）。父窗口传 NULL 时
    // 它枚举顶层窗口，`hWndChildAfter` 是「从这个之后接着找」
    final findWindowEx = user32
        .lookupFunction<
          IntPtr Function(IntPtr, IntPtr, Pointer<Utf16>, Pointer<Utf16>),
          int Function(int, int, Pointer<Utf16>, Pointer<Utf16>)
        >('FindWindowExW');
    final getWindowPid = user32
        .lookupFunction<
          Uint32 Function(IntPtr, Pointer<Uint32>),
          int Function(int, Pointer<Uint32>)
        >('GetWindowThreadProcessId');
    final showWindow = user32
        .lookupFunction<Int32 Function(IntPtr, Int32), int Function(int, int)>(
          'ShowWindow',
        );
    final setForeground = user32
        .lookupFunction<Int32 Function(IntPtr), int Function(int)>(
          'SetForegroundWindow',
        );

    // ⚠️ **必须跳过自己那扇窗。**
    //
    // 这里原来的注释写着「此刻我们自己的窗口还不存在（检查在 runApp
    // 之前）」—— 那是错的：原生窗口由 C++ runner 在 Dart `main()`
    // **之前**就建好并显示了。于是 `FindWindowW('Cortex')` 很可能返回
    // 我们自己那扇（还没画任何东西的）窗，结果是「把白屏窗口置到前台，
    // 然后退出」—— 用户眼里就是点了图标闪一下什么也没发生。
    //
    // 判据用 PID 而不是别的：两个构建的窗口标题与类名完全一样
    // （那正是「用户看不出自己开了两个」的根源），只有进程号能分开。
    final title = 'Cortex'.toNativeUtf16();
    final pidOut = calloc<Uint32>();
    try {
      final self = pid;
      var hwnd = 0;
      while (true) {
        hwnd = findWindowEx(0, hwnd, nullptr, title);
        if (hwnd == 0) return; // 找完了也没有别人的 —— 静默放弃
        getWindowPid(hwnd, pidOut);
        if (pidOut.value != self) break;
      }
      const swRestore = 9; // 最小化的要先还原，直接置前台是无效的
      showWindow(hwnd, swRestore);
      // 我们此刻是前台进程（用户刚双击了图标），把前台**让**出去是
      // Windows 允许的方向 —— 拦的是后台进程抢前台，不拦这个
      setForeground(hwnd);
    } finally {
      calloc.free(title);
      calloc.free(pidOut);
    }
  } on Object {
    // 聚焦失败不值得让退出流程炸掉
  }
}
