@Tags(['live'])
library;

import 'dart:io';

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/theme.dart';
import 'package:cortex_app/features/chat/widgets/memory_drawer.dart';
import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/injected_memory.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/widgets/markdown/code_block.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// Renders a **real** `cortexd` reply through the real widget tree.
///
/// What is asserted here is only what the wire actually guarantees: the text
/// grows, the renderer survives every intermediate (syntactically broken)
/// state, and the per-turn audit reflects what the server sent. It deliberately
/// does *not* require a Rust fence or a non-empty memory set — the live model
/// decides the former and the live retriever, which now abstains, decides the
/// latter. Those invariants are pinned deterministically in
/// `streaming_render_test.dart` and `chat_turn_test.dart`.
///
/// `TestWidgetsFlutterBinding` replaces `HttpOverrides.global` with a stub that
/// returns 400 for everything, so the fetch is wrapped in
/// [HttpOverrides.runWithHttpOverrides] with a genuine `HttpClient` — a
/// zone-local override wins over the global one.
const _baseUrl = 'http://127.0.0.1:8080';

/// 明文 token，与 `live_backend_test` 同一个来源：
///
///     CORTEXD_TOKEN=<明文> flutter test
///
/// 对 `CORTEX_AUTH=disabled` 起的守护进程是 null，那时带上反而是噪声。
final String? _token = () {
  final raw = Platform.environment['CORTEXD_TOKEN']?.trim();
  return (raw == null || raw.isEmpty) ? null : raw;
}();

Future<bool> _daemonUp() async {
  try {
    final socket = await Socket.connect(
      '127.0.0.1',
      8080,
      timeout: const Duration(milliseconds: 600),
    );
    socket.destroy();
    return true;
  } on Object {
    return false;
  }
}

/// A zone-local override that overrides nothing.
///
/// `HttpOverrides.createHttpClient` in the base class constructs the platform
/// client directly, so installing a bare subclass shadows the test binding's
/// stub and restores real networking. Doing it via
/// `runZoned(createHttpClient: () => HttpClient())` instead would recurse
/// infinitely — that factory re-enters the override it is installing.
class _PassthroughHttpOverrides extends HttpOverrides {}

Future<T> _withRealHttp<T>(Future<T> Function() body) =>
    HttpOverrides.runWithHttpOverrides(body, _PassthroughHttpOverrides());

void main() {
  // The capture happens in setUpAll, not inside the test body: `testWidgets`
  // runs under FakeAsync, where a real socket's futures never complete and the
  // test would simply hang.
  var up = false;
  final deltas = <String>[];
  var facts = <InjectedMemory>[];
  var toolCalls = <ToolCall>[];

  setUpAll(() async {
    up = await _daemonUp();
    if (!up) return;

    await _withRealHttp(() async {
      // 带上 token。少了它，一个开着认证的 cortexd 会让 setUpAll 直接 401，
      // 而失败的表现是「整个文件一个用例都没跑」—— 看起来像渲染坏了，
      // 其实是没进门。同 live_backend_test 的读法
      final api = HttpCortexApi(baseUrl: _baseUrl, token: _token);
      try {
        final sessions = await api.sessions();
        await for (final event in api.chat(
          sessionId: sessions.first.id,
          message: '读一下 app/pubspec.yaml，用一两句话说明它声明了哪些依赖',
        )) {
          switch (event) {
            case ChatDeltaEvent(:final text):
              deltas.add(text);
            case ChatMemoryEvent(facts: final f):
              facts = f.map(InjectedMemory.live).toList();
            case ChatToolEvent(:final name, :final summary, :final path):
              toolCalls = ToolCall.merge(toolCalls, name, summary, path: path);
            case ChatErrorEvent(:final message):
              fail('server error: $message');
            default:
              break;
          }
        }
      } finally {
        api.dispose();
      }
    });
  });

  testWidgets('真实回复逐块渲染：文本只增不减，渲染器不因半截语法崩溃', (tester) async {
    if (!up) return markTestSkipped('cortexd not running');

    tester.view.physicalSize = const Size(1200, 1600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    expect(deltas.length, greaterThan(5), reason: '必须是真流式，不是一次性整段');

    Widget frame(String text, bool streaming) => MaterialApp(
      theme: CortexTheme.dark(),
      home: Scaffold(
        body: SingleChildScrollView(
          child: AssistantBlock(
            text: text,
            facts: facts,
            toolCalls: toolCalls,
            streaming: streaming,
          ),
        ),
      ),
    );

    var shown = '';
    var maxCodeBlocks = 0;

    for (final delta in deltas) {
      final previous = shown;
      shown += delta;
      expect(shown.startsWith(previous), isTrue, reason: '增量只能追加');

      await tester.pumpWidget(frame(shown, true));
      await tester.pump(const Duration(milliseconds: 16));

      final blocks = find.byType(CodeBlock).evaluate().length;
      expect(
        blocks,
        greaterThanOrEqualTo(maxCodeBlocks),
        reason: '代码块数量不能减少 —— 那意味着它先渲染成了别的东西再翻转',
      );
      maxCodeBlocks = blocks;
    }

    await tester.pumpWidget(frame(shown, false));
    await tester.pump(const Duration(milliseconds: 50));
    expect(shown.trim(), isNotEmpty);

    // The agent had to read a file to answer, so the audit line must exist and
    // must show one row per invocation, not one per wire event.
    expect(toolCalls, isNotEmpty);
    expect(toolCalls.every((c) => !c.pending), isTrue);
    expect(find.byType(MemoryDrawer), findsOneWidget);

    final toggle = find.textContaining(
      facts.isEmpty ? '本轮工具调用' : '本轮用到的记忆',
    );
    expect(toggle, findsOneWidget);

    await tester.tap(toggle);
    await tester.pump(const Duration(milliseconds: 250));

    if (facts.isEmpty) {
      // Abstention is a correct outcome and must read as one.
      expect(find.textContaining('主动弃权'), findsOneWidget);
    } else {
      expect(find.text(facts.first.fact!.statement), findsOneWidget);
      expect(find.text('出处'), findsWidgets);
    }
  });
}
