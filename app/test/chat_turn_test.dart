import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/memory_drawer.dart';
import 'package:cortex_app/models/injected_memory.dart';
import 'package:cortex_app/models/memory_fact.dart';
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
  group('一轮对话的工具与记忆', () {
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
      expect(message.facts, isNotEmpty);
      expect(message.error, isNull);
    });

    test('检索器弃权时记忆为空，且不算失败', () async {
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
      expect(message.facts, isEmpty, reason: '与已存记忆无关时不应发 memory 事件');
      expect(message.error, isNull, reason: '空记忆是正常结果，绝不能被当成一次失败');
      expect(state.sendError, isNull);
      expect(message.toolCalls.single.result, contains('0 行'));
      expect(message.text, isNotEmpty);
    });
  });

  group('MemoryDrawer', () {
    Widget host(Widget child) => MaterialApp(
      home: Scaffold(body: SingleChildScrollView(child: child)),
    );

    testWidgets('无记忆但有工具调用时，展开说明弃权而非报错', (tester) async {
      await tester.pumpWidget(
        host(
          const MemoryDrawer(
            facts: [],
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

      expect(find.textContaining('主动弃权'), findsOneWidget);
      expect(find.textContaining('返回 0 行', findRichText: true), findsOneWidget);
      expect(
        find.byIcon(Icons.error_outline_rounded),
        findsNothing,
        reason: '弃权不是错误，不能出现错误图标',
      );
    });

    testWidgets('流式期间工具行常驻可见，不必展开', (tester) async {
      await tester.pumpWidget(
        host(
          const MemoryDrawer(
            facts: [],
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

    testWidgets('记忆与工具都有时，标题同时给出两个数量', (tester) async {
      await tester.pumpWidget(
        host(
          MemoryDrawer(
            facts: const [
              InjectedMemory(
                factId: 'f1',
                fact: MemoryFact(id: 'f1', statement: '偏好简洁的回答'),
              ),
            ],
            toolCalls: const [
              ToolCall(name: 'read_file', result: '返回 3 行 / 12 字符'),
              ToolCall(name: 'list_dir', result: '返回 9 行 / 88 字符'),
            ],
          ),
        ),
      );

      expect(find.text('本轮用到的记忆 · 1 条 · 工具 2 次'), findsOneWidget);
    });

    testWidgets('失效的事实仍然列出，只是被标记', (tester) async {
      await tester.pumpWidget(
        host(
          const MemoryDrawer(
            initiallyExpanded: true,
            facts: [
              InjectedMemory(
                factId: 'f1',
                fact: MemoryFact(id: 'f1', statement: '延迟目标是 200ms'),
                channels: ['graph'],
                invalidated: true,
              ),
            ],
          ),
        ),
      );

      expect(
        find.text('延迟目标是 200ms'),
        findsOneWidget,
        reason: '被取代的事实不能被过滤掉 —— 那一轮真的是靠它答的',
      );
      expect(
        find.text('已失效'),
        findsOneWidget,
        reason: '照原样显示而不标出来，等于说这条现在仍然成立',
      );
      expect(
        find.text('graph'),
        findsOneWidget,
        reason: '回放带回了命中的召回路，这是实时事件给不了的信息',
      );
    });

    testWidgets('被抹除的记忆显示占位，而不是整条消失', (tester) async {
      await tester.pumpWidget(
        host(
          const MemoryDrawer(
            initiallyExpanded: true,
            facts: [InjectedMemory(factId: '01JFACTGONE')],
          ),
        ),
      );

      expect(
        find.textContaining('已不可见的记忆'),
        findsOneWidget,
        reason: '藏掉一条就是让这一轮看起来比实际少用了一条记忆 —— 那是篡改回放',
      );
      expect(find.textContaining('01JFACTGONE'), findsOneWidget);
    });

    testWidgets('文件工具行显示服务端给的 path，而不是整串参数', (tester) async {
      await tester.pumpWidget(
        host(
          const MemoryDrawer(
            initiallyExpanded: true,
            facts: [],
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
