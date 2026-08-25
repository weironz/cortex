/// 右栏「终端」页签里那个**真的能敲**的终端。
///
/// # 它连的是什么
///
/// 本机 agent（cortex-local）的 `/local/terminal/{session}`：那头是一个
/// 真 PTY（Windows 上 ConPTY 跑 powershell，unix 上 `$SHELL`），这头是
/// xterm 的终端模拟器。二进制帧是字节流，文本帧是控制消息（resize）——
/// 协议见 `crates/cortex-local/src/terminal.rs` 的模块头。
///
/// # 生命周期
///
/// 部件销毁（切页签、切会话、收起右栏）就关 WS，agent 那侧随之收掉 shell
/// 子进程 —— **不留后台 shell**。重新打开页签就是重开一个 shell，不保留
/// 历史：保留历史意味着 agent 侧要长期挂着进程与缓冲，而「每会话最多一个
/// 终端」的上限就名存实亡了。
library;

import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:web_socket_channel/web_socket_channel.dart';
import 'package:xterm/xterm.dart';

import '../../core/terminal_channel.dart';

class InteractiveTerminal extends StatefulWidget {
  const InteractiveTerminal({
    super.key,
    required this.origin,
    required this.sessionId,
    this.token,
  });

  /// 本机 agent 的 `http://127.0.0.1:PORT`。
  final String origin;

  final String sessionId;

  /// agent 启动时钉住的入站凭据（见 `LocalAgentHandle.pinnedCredential`）。
  final String? token;

  @override
  State<InteractiveTerminal> createState() => _InteractiveTerminalState();
}

class _InteractiveTerminalState extends State<InteractiveTerminal> {
  late Terminal _terminal;
  WebSocketChannel? _channel;
  StreamSubscription<dynamic>? _sub;
  StreamController<List<int>>? _bytes;

  /// 断开的原因；null = 还连着。shell 自己 exit 时 agent 会在 Close 帧里
  /// 带「shell 已退出」，网络断开则是通用的一句 —— 两者要区分开：
  /// 前者点重开就好，后者可能是 agent 死了。
  String? _closed;

  @override
  void initState() {
    super.initState();
    _open();
  }

  void _open() {
    _closed = null;
    // 每次重开都换一个全新的 Terminal：上一个 shell 的屏幕状态（alternate
    // screen、光标位置）对新 shell 全是错的，续着写只会画出一屏叠影
    _terminal = Terminal(maxLines: 10000);
    final channel = connectTerminalChannel(
      origin: widget.origin,
      sessionId: widget.sessionId,
      token: widget.token,
    );
    _channel = channel;

    // 输出走**有状态的** UTF-8 解码：PTY 分块会把多字节字符切成两半，
    // 逐块 utf8.decode 会在每个切口画一个替换字符 —— 中文输出必撞
    final bytes = StreamController<List<int>>();
    _bytes = bytes;
    bytes.stream
        .transform(const Utf8Decoder(allowMalformed: true))
        .listen(_terminal.write);

    // 用户敲的东西 → 二进制帧
    _terminal.onOutput = (data) => channel.sink.add(utf8.encode(data));
    // 尺寸变化 → 文本帧（控制消息）。首次布局就会触发一次，把 agent 侧
    // 的 80x24 初始值顶掉
    _terminal.onResize = (w, h, pw, ph) =>
        channel.sink.add(jsonEncode({'type': 'resize', 'cols': w, 'rows': h}));

    _sub = channel.stream.listen(
      (data) {
        if (data is List<int>) {
          bytes.add(data);
        } else if (data is String) {
          // agent 侧现在不发文本帧；照收不误 —— 协议扩展时旧客户端不该丢字
          bytes.add(utf8.encode(data));
        }
      },
      onDone: () {
        if (!mounted) return;
        final reason = _channel?.closeReason;
        setState(
          () => _closed = (reason == null || reason.isEmpty) ? '连接已断开' : reason,
        );
      },
      onError: (Object e) {
        if (!mounted) return;
        setState(() => _closed = '连不上本机 agent：$e');
      },
    );
  }

  void _teardown() {
    unawaited(_sub?.cancel());
    _sub = null;
    unawaited(_bytes?.close());
    _bytes = null;
    // 关 WS —— agent 那侧收到关闭就收掉 shell 子进程并释放席位
    unawaited(_channel?.sink.close());
    _channel = null;
  }

  @override
  void dispose() {
    _teardown();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    if (_closed case final reason?) {
      return Center(
        child: Padding(
          padding: const EdgeInsets.all(20),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                reason,
                textAlign: TextAlign.center,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
              const SizedBox(height: 10),
              FilledButton.tonalIcon(
                onPressed: () => setState(() {
                  _teardown();
                  _open();
                }),
                icon: const Icon(Icons.refresh_rounded, size: 16),
                label: const Text('重开一个 shell'),
              ),
            ],
          ),
        ),
      );
    }
    return Padding(
      padding: const EdgeInsets.fromLTRB(6, 4, 6, 6),
      child: TerminalView(
        _terminal,
        autofocus: true,
        textStyle: const TerminalStyle(
          fontSize: 12,
          fontFamily: 'JetBrains Mono',
        ),
      ),
    );
  }
}
