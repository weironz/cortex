/// 第四状态 awaiting_confirm —— 「在等你确认」。
///
/// 三态（running / finished / failed）里没有它的位置，而它是唯一
/// 「不处理就永远卡住」的：超时后服务端按拒绝处理，一轮白跑。
/// 这一组钉住它的派生与两处落点的行为（左栏状态点、顶栏 pill —— 颜色
/// 同源 `tokens.warning`，这里钉行为，颜色同源靠代码里只有一个 token）。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/awaiting_confirm_pill.dart';
import 'package:cortex_app/models/pending_confirmation.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/confirm_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

PendingConfirmation _req({required String token, String? sessionId}) =>
    PendingConfirmation(
      token: token,
      tool: 'shell',
      risk: 'execute',
      preview: 'rm -rf /tmp/x',
      deadline: DateTime.now().add(const Duration(seconds: 180)),
      sessionId: sessionId,
    );

ProviderContainer _boot() {
  final c = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_MockConfig.new),
      cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
    ],
  );
  c.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  return c;
}

/// 纯 Dart 测试里等 mock 会话列表载完。
///
/// ⚠️ **widget 测试里不能直接 await 它** —— `testWidgets` 跑在 fake async
/// 里，真 `Future.delayed` 永远不会完成，整个文件静默挂死（assistant_test
/// 里记过这个坑，这个文件 2026-08-24 又踩了一次才写下这行）。
/// widget 测试里包一层：`await tester.runAsync(() => _settle(c))`。
Future<void> _settle(ProviderContainer c) async {
  final deadline = DateTime.now().add(const Duration(seconds: 10));
  while (c.read(chatControllerProvider).sessionsLoading) {
    if (DateTime.now().isAfter(deadline)) fail('等会话列表超时');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

void main() {
  group('awaitingConfirmSessionsProvider', () {
    test('从待确认队列派生出会话 id，同一会话多条确认只算一次', () {
      final c = _boot();
      addTearDown(c.dispose);

      final confirm = c.read(confirmControllerProvider.notifier);
      confirm.offer(_req(token: 't1', sessionId: 'ses-a'));
      confirm.offer(_req(token: 't2', sessionId: 'ses-a'));
      confirm.offer(_req(token: 't3', sessionId: 'ses-b'));

      expect(c.read(awaitingConfirmSessionsProvider), {
        'ses-a',
        'ses-b',
      }, reason: '状态点按会话画，一条会话上挂两个确认仍然是一个点');
    });

    test('没有 sessionId 的确认不进集合，而不是炸掉或算成 null', () {
      final c = _boot();
      addTearDown(c.dispose);

      c
          .read(confirmControllerProvider.notifier)
          .offer(_req(token: 't1', sessionId: null));

      expect(
        c.read(awaitingConfirmSessionsProvider),
        isEmpty,
        reason: '旧服务端的确认事件可能不带 session_id —— 它照常在面板里出现，只是左栏没有点可挂',
      );
    });
  });

  group('顶栏「等你确认」pill', () {
    testWidgets('别的会话在等时出现，点一下直达', (tester) async {
      final c = _boot();
      await tester.runAsync(() => _settle(c));

      // 确认挂在**不是当前**的会话上
      final sessions = c.read(chatControllerProvider).sessions;
      final other = sessions
          .firstWhere(
            (s) => s.id != c.read(chatControllerProvider).activeSessionId,
          )
          .id;
      c
          .read(confirmControllerProvider.notifier)
          .offer(_req(token: 't1', sessionId: other));

      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: const MaterialApp(
            home: Scaffold(body: Center(child: AwaitingConfirmPill())),
          ),
        ),
      );
      await tester.pump();

      expect(find.text('等你确认 · 1'), findsOneWidget);

      await tester.tap(find.text('等你确认 · 1'));
      await tester.pump();
      expect(
        c.read(chatControllerProvider).activeSessionId,
        other,
        reason: 'pill 的全部意义就是「点一下直达卡住的那条」—— 只报数不带路是半个功能',
      );
      // 就地 dispose 而不是 addTearDown：确认队列非空时秒表在走，
      // fake async 的收尾断言（!timersPending）跑在 tearDown 之前 ——
      // confirm_test 里全部 widget 用例都是这么收的
      c.dispose();
    });

    testWidgets('⚠️ 正停在那条会话上时不画', (tester) async {
      final c = _boot();
      await tester.runAsync(() => _settle(c));

      final active = c.read(chatControllerProvider).activeSessionId!;
      c
          .read(confirmControllerProvider.notifier)
          .offer(_req(token: 't1', sessionId: active));

      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: const MaterialApp(
            home: Scaffold(body: Center(child: AwaitingConfirmPill())),
          ),
        ),
      );
      await tester.pump();

      expect(
        find.textContaining('等你确认'),
        findsNothing,
        reason: '确认面板就在眼前时，一颗指向「你正看着的东西」的 pill 是噪音',
      );
      c.dispose();
    });
  });
}
