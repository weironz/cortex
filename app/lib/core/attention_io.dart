/// 桌面那一侧。见 `attention.dart`。
library;

import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

/// 让任务栏按钮闪起来，直到用户点它。
///
/// 回 `false` = 这个平台上做不到（macOS / Linux 桌面），调用方据此
/// 什么都别声称。**不假装成功**：静默当作做到了是最难自查的一类 bug。
///
/// [title] 在这一侧用不上（Windows 闪的是窗口本身，不带文字），
/// 保留它是为了让两侧同签名 —— 调用方因此不需要任何 `kIsWeb`。
Future<bool> callAttention(String title, String body) async {
  if (!Platform.isWindows) return false;
  try {
    final user32 = DynamicLibrary.open('user32.dll');
    // 自己这扇窗：`GetActiveWindow` 只在本线程有活动窗口时给得出，
    // 而 Flutter 的 Dart 跑在自己的线程上 —— 所以走 `FindWindowExW`
    // 按标题 + PID 找，与 `single_instance_io.dart` 同一条路
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
    final getForeground = user32
        .lookupFunction<IntPtr Function(), int Function()>(
          'GetForegroundWindow',
        );
    final flash = user32
        .lookupFunction<
          Int32 Function(Pointer<_FlashInfo>),
          int Function(Pointer<_FlashInfo>)
        >('FlashWindowEx');

    final name = 'Cortex'.toNativeUtf16();
    final pidOut = calloc<Uint32>();
    final info = calloc<_FlashInfo>();
    try {
      // 这一次找的是**自己那扇**（与单实例守门相反：那边要跳过自己）
      var hwnd = 0;
      while (true) {
        hwnd = findWindowEx(0, hwnd, nullptr, name);
        if (hwnd == 0) return false; // 窗口还没建出来 —— 没什么可闪的
        getWindowPid(hwnd, pidOut);
        if (pidOut.value == pid) break;
      }

      // ⚠️ **已经在前台就不闪。** 用户正看着这扇窗的时候闪任务栏，
      // 是在替一件他已经看见的事情制造噪音 —— 而这个能力的全部意义
      // 是「人不在的时候把话带到」。判据是「前台窗口是不是我们这扇」，
      // 不是「应用有没有在跑」
      if (getForeground() == hwnd) return false;

      info.ref.cbSize = sizeOf<_FlashInfo>();
      info.ref.hwnd = hwnd;
      // FLASHW_ALL(3) | FLASHW_TIMERNOFG(12) —— 闪窗口与任务栏按钮，
      // **一直闪到窗口被带到前台为止**。给固定次数的话，人离开五分钟
      // 回来时它早停了，而那正是这条要覆盖的情形
      info.ref.dwFlags = 3 | 12;
      info.ref.uCount = 0;
      info.ref.dwTimeout = 0; // 0 = 用系统默认的闪烁间隔
      flash(info);
      return true;
    } finally {
      calloc.free(name);
      calloc.free(pidOut);
      calloc.free(info);
    }
  } on Object {
    // 提醒失败不值得让一轮对话炸掉 —— 它是锦上添花
    return false;
  }
}

/// `FLASHWINFO`。字段顺序与 Win32 头文件**必须一字不差** ——
/// 错一个字段的症状是 `cbSize` 对不上，`FlashWindowEx` 静默返回 0。
final class _FlashInfo extends Struct {
  @Uint32()
  external int cbSize;
  @IntPtr()
  external int hwnd;
  @Uint32()
  external int dwFlags;
  @Uint32()
  external int uCount;
  @Uint32()
  external int dwTimeout;
}
