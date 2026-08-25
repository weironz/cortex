/// 桌面端：真的能连本地 agent 的终端 WS。见 `terminal_channel.dart`。
library;

import 'dart:io';

import 'package:web_socket_channel/io.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

/// 桌面端可以有交互终端（前提是本地 agent 活着 —— 那个判据在调用方，
/// 这里只回答「这个构建连不连得了」）。
const bool kInteractiveTerminalSupported = true;

/// 连上 `cortex-local` 的 `/local/terminal/{session}`。
///
/// [origin] 是本地 agent 的 `http://127.0.0.1:PORT`；[token] 是它启动时
/// 钉住的那把入站凭据（`LocalAgentHandle.pinnedCredential`）——
/// **不是**用户的 access token，那把每 15 分钟就换，而 agent 只认启动时的。
WebSocketChannel connectTerminalChannel({
  required String origin,
  required String sessionId,
  String? token,
}) {
  final ws = origin.replaceFirst(RegExp('^http'), 'ws');
  return IOWebSocketChannel.connect(
    Uri.parse('$ws/local/terminal/${Uri.encodeComponent(sessionId)}'),
    headers: token == null
        ? null
        : {HttpHeaders.authorizationHeader: 'Bearer $token'},
  );
}
