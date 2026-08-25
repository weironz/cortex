/// 交互终端的 WS 通道 —— 第六个平台接缝。
///
/// # 为什么又要一个接缝
///
/// 终端 WS 必须带 `Authorization` 头（本地 agent 的入站认证），而带头的
/// `IOWebSocketChannel` 只在 `dart:io` 上存在 —— 浏览器根本不允许给 WS
/// 握手加请求头。所以这条连接**只能**在桌面端建，Web 侧连编译都不该编到它。
///
/// 与 `local_agent.dart` 同一套论证：两侧同名导出，调用方不写 `kIsWeb`。
/// Web 侧 `kInteractiveTerminalSupported` 恒 false，右栏据此退回只读的
/// 命令记录（约束 2：没有本地 shell 就不摆一个连不上的终端）。
library;

export 'terminal_channel_io.dart'
    if (dart.library.js_interop) 'terminal_channel_web.dart';
