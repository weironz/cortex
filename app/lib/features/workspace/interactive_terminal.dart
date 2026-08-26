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
import '../../core/theme.dart';

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

  /// 自己拿着焦点节点，**不用 `autofocus`**。
  ///
  /// `autofocus` 的语义是「这个作用域里还没人拿着焦点时才要」。而进到这个
  /// 页签的唯一路径是点上面那颗「终端」页签 chip —— 那颗 InkWell 是可聚焦
  /// 的，点完焦点在它身上，于是 `autofocus` 判定「已经有人拿着了」，
  /// 直接放弃。表现就是：终端画出来了、输出也在流，但敲什么都没反应。
  ///
  /// 拿着节点自己在首帧之后 `requestFocus()`，这条判断就不存在了。
  final FocusNode _focus = FocusNode(debugLabel: 'terminal');

  /// 断开的原因；null = 还连着。shell 自己 exit 时 agent 会在 Close 帧里
  /// 带「shell 已退出」，网络断开则是通用的一句 —— 两者要区分开：
  /// 前者点重开就好，后者可能是 agent 死了。
  String? _closed;

  @override
  void initState() {
    super.initState();
    _open();
    // 首帧之后再要：这一刻部件还没进树，节点要不到焦点
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _focus.requestFocus();
    });
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
    _focus.dispose();
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
    return GestureDetector(
      // 点这一块的**任何地方**都把焦点抓回来，包括 xterm 自己那圈内边距
      // 之外的留白 —— 一个「看着像终端、点了却敲不进去」的角落，用户
      // 只会得出「这个终端是坏的」
      behavior: HitTestBehavior.opaque,
      onTapDown: (_) => _focus.requestFocus(),
      child: Padding(
        padding: const EdgeInsets.fromLTRB(6, 4, 6, 6),
        child: TerminalView(
          _terminal,
          focusNode: _focus,
          theme: terminalTheme(theme),
          textStyle: const TerminalStyle(
            fontSize: 12,
            fontFamily: 'JetBrains Mono',
          ),
        ),
      ),
    );
  }
}

/// 把应用主题翻成 xterm 的调色板。
///
/// # 为什么必须给，不能用默认
///
/// xterm 的 `TerminalThemes.defaultTheme` 是**写死的 VS Code 深色**
/// （底 `#1E1E1E`、字 `#CCCCCC`）。不传 `theme:` 的后果是右栏里挖出一块
/// 黑，浅色主题下尤其突兀 —— 而且它与旁边两个页签（文件 / 本轮改动）
/// 用的是同一条栏，那两页跟着主题走，只有这一页不跟。
///
/// # 底色为什么取 `sidebar` 而不是 `surface`
///
/// 这一栏整个被 `Container(color: tokens.sidebar)` 垫着（见 `AppShell`
/// 与 `showRightRailSheet`）。取 `surface` 会画出一块比栏底浅一点的方块，
/// 看起来像个没对齐的卡片。
///
/// # 16 色为什么不跟着主题算
///
/// 它们是**协议规定的语义**：`ls` 用绿标可执行、`git` 用红标删除。
/// 按主题的强调色算出来的「绿」会让 `git diff` 变得读不懂。所以这里
/// 照抄 VS Code 的深/浅两套 —— 换的只是那一套，不是配色规则。
TerminalTheme terminalTheme(ThemeData theme) {
  final scheme = theme.colorScheme;
  final dark = theme.brightness == Brightness.dark;
  return TerminalTheme(
    cursor: scheme.primary,
    // 选中态跟系统走：这一块的选中与聊天区的选中是同一件事
    selection: scheme.primary.withValues(alpha: 0.28),
    foreground: scheme.onSurface,
    background: theme.cortex.sidebar,
    black: const Color(0xFF000000),
    red: const Color(0xFFCD3131),
    green: dark ? const Color(0xFF0DBC79) : const Color(0xFF00BC00),
    yellow: dark ? const Color(0xFFE5E510) : const Color(0xFF949800),
    blue: dark ? const Color(0xFF2472C8) : const Color(0xFF0451A5),
    magenta: dark ? const Color(0xFFBC3FBC) : const Color(0xFFBC05BC),
    cyan: dark ? const Color(0xFF11A8CD) : const Color(0xFF0598BC),
    white: dark ? const Color(0xFFE5E5E5) : const Color(0xFF555555),
    brightBlack: const Color(0xFF666666),
    brightRed: dark ? const Color(0xFFF14C4C) : const Color(0xFFCD3131),
    brightGreen: dark ? const Color(0xFF23D18B) : const Color(0xFF14CE14),
    brightYellow: dark ? const Color(0xFFF5F543) : const Color(0xFFB5BA00),
    brightBlue: dark ? const Color(0xFF3B8EEA) : const Color(0xFF0451A5),
    brightMagenta: dark ? const Color(0xFFD670D6) : const Color(0xFFBC05BC),
    brightCyan: dark ? const Color(0xFF29B8DB) : const Color(0xFF0598BC),
    brightWhite: dark ? const Color(0xFFFFFFFF) : const Color(0xFFA5A5A5),
    // 搜索命中那三色不跟主题：它们要在**任何**底色上都跳出来，
    // 这正是「例外」该有的表达
    searchHitBackground: const Color(0xFFFFFF2B),
    searchHitBackgroundCurrent: const Color(0xFF31FF26),
    searchHitForeground: const Color(0xFF000000),
  );
}
