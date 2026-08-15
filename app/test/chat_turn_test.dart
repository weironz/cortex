import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/turn_drawer.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

Future<void> _until(
  bool Function() condition, {
  String reason = '',
  Duration timeout = const Duration(seconds: 30),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 15));
  }
}

ProviderContainer _boot() {
  final container = ProviderContainer(
    overrides: [appConfigProvider.overrideWith(_MockConfig.new)],
  );
  container.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  return container;
}

void main() {
  group('一轮对话的工具轨迹', () {
    test('两条 tool 事件折叠成一次调用', () async {
      final container = _boot();
      addTearDown(container.dispose);

      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );
      await container
          .read(chatControllerProvider.notifier)
          .send('Rust async trait 怎么选');
      await _until(
        () => container.read(chatControllerProvider).streaming == null,
        reason: '流式结束',
      );

      final message = container
          .read(chatControllerProvider)
          .activeTranscript
          .last;
      expect(
        message.toolCalls,
        hasLength(1),
        reason: '一次 memory_search 发了调用与返回两条事件，UI 上必须只占一行',
      );
      expect(message.toolCalls.single.arguments, startsWith('(query='));
      expect(message.toolCalls.single.result, startsWith('返回 '));
      expect(message.toolCalls.single.pending, isFalse);
      expect(message.error, isNull);
    });

    /// `memory_search` **仍然是一条真实的工具调用** —— agent 那侧照旧去问记忆
    /// 服务。删掉的只是「把召回的事实画在界面上」那一半，工具行没有跟着走。
    ///
    /// 这条钉的就是这个区分：删界面的时候顺手把工具行也删掉，症状是用户再也
    /// 看不出那一轮到底查没查记忆。
    test('memory_search 的工具行照旧出现', () async {
      final container = _boot();
      addTearDown(container.dispose);

      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );
      await container.read(chatControllerProvider.notifier).send('帮我看看窗外下没下雨');
      await _until(
        () => container.read(chatControllerProvider).streaming == null,
        reason: '流式结束',
      );

      final state = container.read(chatControllerProvider);
      final message = state.activeTranscript.last;
      expect(message.toolCalls.single.name, 'memory_search');
      expect(message.error, isNull);
      expect(state.sendError, isNull);
      expect(message.text, isNotEmpty);
    });
  });

  group('TurnDrawer', () {
    Widget host(Widget child) => MaterialApp(
      home: Scaffold(body: SingleChildScrollView(child: child)),
    );

    testWidgets('收起时只给一行，展开后才是工具明细', (tester) async {
      await tester.pumpWidget(
        host(
          const TurnDrawer(
            toolCalls: [
              ToolCall(
                name: 'memory_search',
                arguments: '(query=天气)',
                result: '返回 0 行 / 0 字符',
              ),
            ],
          ),
        ),
      );

      expect(find.text('本轮工具调用 · 1 次'), findsOneWidget);
      await tester.tap(find.text('本轮工具调用 · 1 次'));
      await tester.pumpAndSettle();

      expect(find.textContaining('返回 0 行', findRichText: true), findsOneWidget);
      expect(
        find.byIcon(Icons.error_outline_rounded),
        findsNothing,
        reason: '返回 0 行不是失败 —— 检索器弃权是正常结果，不该出现错误图标',
      );
    });

    testWidgets('流式期间工具行常驻可见，不必展开', (tester) async {
      await tester.pumpWidget(
        host(
          const TurnDrawer(
            streaming: true,
            toolCalls: [ToolCall(name: 'read_file', arguments: '(path=a.rs)')],
          ),
        ),
      );

      // Still running: no result yet, and the row is on screen without a tap.
      expect(
        find.textContaining('read_file', findRichText: true),
        findsOneWidget,
      );
      expect(find.byType(CircularProgressIndicator), findsOneWidget);
    });

    testWidgets('标题给出工具次数', (tester) async {
      await tester.pumpWidget(
        host(
          const TurnDrawer(
            toolCalls: [
              ToolCall(name: 'read_file', result: '返回 3 行 / 12 字符'),
              ToolCall(name: 'list_dir', result: '返回 9 行 / 88 字符'),
            ],
          ),
        ),
      );

      expect(find.text('本轮工具调用 · 2 次'), findsOneWidget);
    });

    /// 一轮里一次工具都没调时，这个抽屉**整个不出现**。
    ///
    /// 此前它在「没有记忆但有工具」和「有记忆没有工具」之间都要显示，
    /// 所以空态是有意义的。现在只剩工具这一个维度，空态就该是不占位 ——
    /// 留一行「本轮工具调用 · 0 次」是给每条纯聊天的回答都挂一个没用的把手。
    testWidgets('一次工具都没有时整个不渲染', (tester) async {
      await tester.pumpWidget(host(const TurnDrawer()));
      expect(find.textContaining('本轮工具调用'), findsNothing);
    });

    testWidgets('文件工具行显示服务端给的 path，而不是整串参数', (tester) async {
      await tester.pumpWidget(
        host(
          const TurnDrawer(
            initiallyExpanded: true,
            toolCalls: [
              ToolCall(
                name: 'write_file',
                path: 'src/notes.md',
                arguments: '(content=…, path=src/notes.md)',
                result: '已写入 412 字节',
              ),
            ],
          ),
        ),
      );

      expect(
        find.textContaining('src/notes.md', findRichText: true),
        findsOneWidget,
      );
      expect(
        find.textContaining('content=', findRichText: true),
        findsNothing,
        reason: '有 path 时参数串就该让位 —— 它挤在 content 旁边等于看不见',
      );
    });
  });
}
