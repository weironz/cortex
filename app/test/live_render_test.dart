@Tags(['live'])
library;

import 'dart:io';

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/theme.dart';
import 'package:cortex_app/features/chat/widgets/turn_drawer.dart';
import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/widgets/markdown/code_block.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
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
// 同 live_backend_test：打边缘那个口，不是任何单个服务。理由见那边
const _baseUrl = 'http://127.0.0.1:5173';

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
      5173,
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
  var toolCalls = <ToolCall>[];

  setUpAll(() async {
    up = await _daemonUp();
    if (!up) return;

    // 没抓到工具调用就再来一轮。
    //
    // 这条用例测的是**渲染**：下面要拿一份真回复去喂真组件树。而「模型这一轮
    // 到底调不调工具」是模型的自由 —— 让渲染的正确性挂在那个骰子上，
    // 结果是一条三次里红一次的测试，而三次里红一次的测试等于没有测试
    //（人只会开始无视它）。真答不出来的话第二轮照样会红，只是不再随机。
    for (var attempt = 0; attempt < 2; attempt++) {
      deltas.clear();
      toolCalls = <ToolCall>[];
      await _withRealHttp(() async {
        // 带上 token。少了它，一个开着认证的 cortexd 会让 setUpAll 直接 401，
        // 而失败的表现是「整个文件一个用例都没跑」—— 看起来像渲染坏了，
        // 其实是没进门。同 live_backend_test 的读法
        final api = HttpCortexApi(baseUrl: _baseUrl, token: _token);
        try {
          await for (final event in api.chat(
            // 全新会话，不是 `sessions.first`：那条会话里已经躺着别的用例问过的
            // 同一件事，模型直接照着历史答，一次工具都不调 —— 而下面要断言的
            // 正是审计行里有那次调用。同 `live_backend_test` 里的 `POST /chat`
            sessionId:
                'live-render-${DateTime.now().millisecondsSinceEpoch}-$attempt',
            // ⚠️ **这里点名的工具必须是当下真的存在的那个。**
            //
            // 这条用例要的是一次**成功**的调用，好让审计行有东西可画。
            // 从前它点名 `memory_search` —— 而那个工具 2026-08-17
            // 随记忆层一起拆掉了，于是模型只能回一句「我没有这个工具」，
            // 一次调用都不发，这条从那天起一直是红的。
            //
            // 用 `write_file`，与 `live_backend_test` 里那条「工具事件成对」
            // 同一个 —— 两处点同一个工具，下线时一起红，不会漏掉一处。
            message: '用 write_file 工具往 note.txt 写一行 hello，然后一句话说说结果。',
          )) {
            switch (event) {
              case ChatDeltaEvent(:final text):
                deltas.add(text);
              case ChatToolEvent(:final name, :final summary, :final path):
                toolCalls = ToolCall.merge(
                  toolCalls,
                  name,
                  summary,
                  path: path,
                );
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
      if (toolCalls.isNotEmpty && deltas.length > 5) return;
    }
  });

  testWidgets('真实回复逐块渲染：文本只增不减，渲染器不因半截语法崩溃', (tester) async {
    if (!up) return markTestSkipped('cortexd not running');

    tester.view.physicalSize = const Size(1200, 1600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    expect(deltas.length, greaterThan(5), reason: '必须是真流式，不是一次性整段');

    // 包一层 ProviderScope：气泡里的「从这里分叉」是个 Consumer，
    // 没有作用域会在 build 时抛 `No ProviderScope found`。这里不覆写任何
    // provider —— 这个用例验的是渲染，不是分叉行为，默认实现就够
    Widget frame(String text, bool streaming) => ProviderScope(
      child: MaterialApp(
        theme: CortexTheme.dark(),
        home: Scaffold(
          body: SingleChildScrollView(
            child: AssistantBlock(
              text: text,
              toolCalls: toolCalls,
              streaming: streaming,
            ),
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

    // 这一轮点名要了一次工具，所以审计行必须在，且**每次调用一行**
    // （不是每条线上事件一行 —— 一次调用发两条）。
    expect(toolCalls, isNotEmpty);
    expect(toolCalls.every((c) => !c.pending), isTrue);
    expect(find.byType(TurnDrawer), findsOneWidget);

    final toggle = find.textContaining('本轮工具调用');
    expect(toggle, findsOneWidget);

    await tester.tap(toggle);
    await tester.pump(const Duration(milliseconds: 250));

    // 展开之后那次调用的名字必须看得见 —— 抽屉的全部用处就是这个。
    expect(
      find.textContaining(toolCalls.first.name, findRichText: true),
      findsWidgets,
    );
  });
}
