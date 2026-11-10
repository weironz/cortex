import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/features/sessions/session_list.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/models/message_persistence.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

void main() {
  test('保存状态保留宿主证据，旧宿主和未知状态均不推定远端已保存', () {
    for (final entry in {
      'local_only': MessagePersistence.localOnly,
      'pending': MessagePersistence.pending,
      'remote': MessagePersistence.remote,
      'future_state': MessagePersistence.unknown,
    }.entries) {
      final event =
          ChatEvent.fromJson({
                'type': 'done',
                'episode_id': 'ep',
                'persistence': entry.key,
              })
              as ChatDoneEvent;
      expect(event.persistence, entry.value, reason: '完成事件不能丢失保存位置');
      final session = ChatSession.fromJson({
        'id': 's',
        'persistence': entry.key,
      });
      expect(session.persistence, entry.value, reason: '历史重载也须保留相同证据');
      expect(
        session.copyWith(title: '更新标题').persistence,
        entry.value,
        reason: '本地编辑不能抹掉保存状态',
      );
    }
    expect(
      (ChatEvent.fromJson({'type': 'done', 'episode_id': 'ep'})
              as ChatDoneEvent)
          .persistence,
      isNull,
    );
    expect(MessagePersistence.fromWire(42), MessagePersistence.unknown);
  });

  test('复制消息保留保存状态和模型归因', () {
    final message = ChatMessage(
      id: 'm',
      role: MessageRole.assistant,
      text: '内容',
      createdAt: DateTime.utc(2026),
      models: const ['test-model'],
      persistence: MessagePersistence.pending,
    );
    final copied = message.copyWith(text: '修改内容');
    expect(copied.persistence, MessagePersistence.pending);
    expect(copied.models, ['test-model'], reason: '修改正文不能丢掉模型归因');
  });

  for (final status in [null, ...MessagePersistence.values]) {
    testWidgets('会话菜单按保存证据限制远端分叉：${status?.name ?? "legacy"}', (tester) async {
      var calls = 0;
      await tester.pumpWidget(
        ProviderScope(
          overrides: [appConfigProvider.overrideWith(_MockConfig.new)],
          child: MaterialApp(
            home: Scaffold(
              body: SessionTileMenu(
                archived: false,
                enabled: true,
                canMove: false,
                pinned: false,
                remoteHistoryReady: MessagePersistence.canForkRemote(status),
                onRename: () {},
                onFork: () => calls++,
                onTogglePin: () {},
                onToggleArchive: () {},
                onMove: () {},
                onExport: (_) {},
              ),
            ),
          ),
        ),
      );
      await tester.tap(find.byType(SessionTileMenu));
      await tester.pumpAndSettle();
      final item = tester.widget<PopupMenuItem<String>>(
        find.widgetWithText(PopupMenuItem<String>, '分叉会话'),
      );
      final allowed = status == null || status == MessagePersistence.remote;
      expect(item.enabled, allowed, reason: '会话菜单不能绕过消息入口的保存位置限制');
      if (allowed) {
        await tester.tap(find.text('分叉会话'));
        await tester.pumpAndSettle();
        expect(calls, 1);
      } else {
        expect(calls, 0);
      }
    });
  }

  for (final status in [
    MessagePersistence.localOnly,
    MessagePersistence.pending,
    MessagePersistence.unknown,
  ]) {
    testWidgets('${status.name} 明示保存位置且不允许使用未同步 episode 远端分叉', (tester) async {
      await tester.pumpWidget(
        ProviderScope(
          overrides: [appConfigProvider.overrideWith(_MockConfig.new)],
          child: MaterialApp(
            home: Scaffold(
              body: SingleChildScrollView(
                child: MessageBubble(
                  message: ChatMessage(
                    id: 'm',
                    role: MessageRole.assistant,
                    text: '内容',
                    createdAt: DateTime.utc(2026),
                    episodeId: 'local-ep',
                    persistence: status,
                  ),
                ),
              ),
            ),
          ),
        ),
      );
      expect(find.text(status.label), findsOneWidget);
      final fork = find.byTooltip('等待远端确认保存后再分叉');
      expect(fork, findsOneWidget);
      final button = find.ancestor(of: fork, matching: find.byType(IconButton));
      expect(button, findsOneWidget);
      expect(tester.widget<IconButton>(button).onPressed, isNull);
      expect(tester.takeException(), isNull);
    });
  }
}
