/// 桌面端：把目录交给系统文件管理器。见 `reveal_in_file_manager.dart`。
library;

import 'dart:io';

/// 桌面端画那个按钮。「会话有没有本机工作区」是另一半判据，在调用方。
const bool kCanRevealInFileManager = true;

/// 在系统文件管理器里打开 [path]（一个目录）。
///
/// # 为什么是 `Process.start` 而不是 url_launcher 的 `file://`
///
/// `launchUrl(file://…)` 在 Windows 上会把目录交给「默认关联程序」——
/// 那不保证是资源管理器（装过某些压缩软件的机器上是它们）。三条命令
/// 各平台钉死，行为才可预期。
///
/// detached：文件管理器是用户的进程，不该挂在我们下面 —— 挂着的话
/// 退出应用会连着用户正看着的窗口一起收走（Linux 上 xdg-open 的某些
/// 实现确实如此）。
///
/// 失败静默（返回 false）：目录被删了、xdg-open 不存在……这些都不值得
/// 弹一个错误框 —— 用户看一眼没开起来自然会再点，而树那侧对「目录没了」
/// 另有整块提示。
Future<bool> revealInFileManager(String path) async {
  final (exe, args) = Platform.isWindows
      ? ('explorer.exe', [path])
      : Platform.isMacOS
      ? ('open', [path])
      : ('xdg-open', [path]);
  try {
    await Process.start(exe, args, mode: ProcessStartMode.detached);
    return true;
  } on ProcessException {
    return false;
  }
}
