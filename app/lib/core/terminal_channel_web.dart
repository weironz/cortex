/// Web：没有本地 agent，也没有能带认证头的 WS。见 `terminal_channel.dart`。
library;

import 'package:web_socket_channel/web_socket_channel.dart';

/// 浏览器构建永远没有交互终端 —— 本地 shell 不存在。
const bool kInteractiveTerminalSupported = false;

/// 编译期占位。调用方先看 [kInteractiveTerminalSupported] 再调；
/// 真走到这里就是判据被绕过了，抛出来比静默返回一条死通道容易查。
WebSocketChannel connectTerminalChannel({
  required String origin,
  required String sessionId,
  String? token,
}) => throw UnsupportedError('Web 端没有本地 shell，交互终端不可用');
