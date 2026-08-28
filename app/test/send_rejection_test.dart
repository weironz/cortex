/// 拒收一条消息必须**出声**，且用户的文字不能丢。
///
/// # 案发现场（2026-08-29）
///
/// 用户在白纸上打字、按 Enter，「发都发不出去，直接静默消失」。链条是：
///
/// 1. 另一条会话有一轮没收尾（当晚生产在 IO 风暴里，SSE 挂着不吐事件）；
/// 2. composer 的闸只看**当前会话**（`isStreamingActive`，白纸上恒 false）
///    → 放行并**清空输入框**；
/// 3. `ChatController.send` 的闸看**全局**（`streaming != null`）
///    → 静默 `return`。
///
/// 两处判据不一致，缝里没有任何输出 —— 消息凭空没了。修法不是把两个闸
/// 对齐成一个（一次一轮是有意的设计），而是**拒收必须可见**：
/// `send` 回 `false` 并写 `sendError`，composer 靠返回值把文字还回去。
library;

import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/features/chat/widgets/message_composer.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

ProviderContainer _boot() {
  final c = ProviderContainer(
    overrides: [appConfigProvider.overrideWith(_MockConfig.new)],
  );
  c.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  return c;
}

Future<void> _settle(ProviderContainer c) async {
  final deadline = DateTime.now().add(const Duration(seconds: 10));
  while (c.read(chatControllerProvider).sessionsLoading) {
    if (DateTime.now().isAfter(deadline)) fail('等开机那次会话列表超时');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

void main() {
  group('别的会话在流时，白纸上的发送被拒收但不静默', () {
    test('send 回 false、sendError 说清在哪儿、不建会话不丢转录', () async {
      final c = _boot();
      addTearDown(c.dispose);
      await _settle(c);

      final ctrl = c.read(chatControllerProvider.notifier);
      // 在会话 A 里开一轮流（mock 数据源会立刻开始流式回答）
      final a = ctrl.createSession();
      final accepted = await ctrl.send('第一条，占住流');
      expect(accepted, isTrue, reason: '空闲时发送必须被收下');
      expect(
        c.read(chatControllerProvider).streaming,
        isNotNull,
        reason: '前置不成立：mock 的流没有开起来，后面验不到「占线拒收」',
      );

      // 白纸上再发
      ctrl.startNewChat();
      final before = c.read(chatControllerProvider).sessions.length;
      final second = await ctrl.send('白纸上这条不该被吞');

      expect(second, isFalse, reason: '占线时必须拒收 —— 收下会两轮并跑');
      final s = c.read(chatControllerProvider);
      expect(s.sendError, isNotNull, reason: '拒收必须出声。静默 return 正是「消息凭空消失」的那半');
      expect(
        s.sessions.length,
        before,
        reason: '拒收不该把白纸兑现成一条空会话 —— 那是惰性化要防的形状',
      );
      // 占着流的那条会话 A 的转录里，不该混进白纸上这句
      expect(
        (s.transcripts[a]?.messages ?? const []).where(
          (m) => m.text.contains('白纸上这条'),
        ),
        isEmpty,
        reason: '拒收的消息不许落进别的会话的转录',
      );
    });
  });

  group('composer 在拒收时把文字还回输入框', () {
    testWidgets('onSend 回 false → 文本回来；回 true → 保持清空', (tester) async {
      for (final (accepted, expectAfter) in [(false, '要还回来的话'), (true, '')]) {
        await tester.pumpWidget(
          ProviderScope(
            child: MaterialApp(
              home: Scaffold(
                body: MessageComposer(
                  streaming: false,
                  onSend: (_, _) async => accepted,
                  onStop: () {},
                  ensureSession: () => 's-1',
                ),
              ),
            ),
          ),
        );
        await tester.enterText(find.byType(TextField), '要还回来的话');
        await tester.sendKeyEvent(LogicalKeyboardKey.enter);
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 20));
        final field = tester.widget<TextField>(find.byType(TextField));
        expect(
          field.controller?.text ?? '',
          expectAfter,
          reason: accepted
              ? '收下之后输入框该保持清空 —— 还回去会让人发两遍'
              : '拒收之后文字必须还回输入框 —— 丢了就是「静默消失」的另一半',
        );
      }
    });
  });
}
