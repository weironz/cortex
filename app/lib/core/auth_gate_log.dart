/// 「被送回登录页」的落盘日志 —— 平台缝之一。
///
/// 桌面端写进 agent 的启动诊断文件（`agent-launch.jsonl`，跨进程活着，
/// 崩溃循环与被踢的时间线要凑在同一个文件里才看得出因果）；
/// Web 没有文件系统，什么都不做 —— 那里 `debugPrint` 进浏览器控制台，
/// 用户报障时截图就带上了。
///
/// 与 `local_agent.dart` 同一个理由用条件导出而不是 `kIsWeb`：
/// `dart:io` 在 Web 上是编译期就不存在的，判运行时常量救不了 import。
library;

export 'auth_gate_log_io.dart'
    if (dart.library.js_interop) 'auth_gate_log_web.dart';
