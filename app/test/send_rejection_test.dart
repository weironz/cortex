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
  group('别的会话在流时', () {
    /// **照常收下 —— 两条会话可以并跑。**
    ///
    /// 这条用例 2026-09-02 之前断言的是相反的行为（拒收）。用户实报：
    /// 「怎么同时只能运行一个会话，不合理啊」—— 他是对的，而且那纯粹是
    /// 客户端自己加的限制：服务端那侧的轮次登记簿本来就是
    /// `HashMap<session_id, Slot>`，两条会话并跑一直是允许的。
    test('照常收下，两条会话各跑各的', () async {
      final c = _boot();
      addTearDown(c.dispose);
      await _settle(c);

      final ctrl = c.read(chatControllerProvider.notifier);
      // 在会话 A 里开一轮流（mock 数据源会立刻开始流式回答）
      final a = ctrl.createSession();
      expect(await ctrl.send('第一条，占住流'), isTrue, reason: '空闲时发送必须被收下');
      expect(
        c.read(chatControllerProvider).streamingTurns[a],
        isNotNull,
        reason: '前置不成立：mock 的流没有开起来，后面验不到并跑',
      );

      // 白纸上再发 —— 另一条会话
      ctrl.startNewChat();
      final second = await ctrl.send('白纸上这条该照常发出去');
      expect(second, isTrue, reason: '别的会话在跑就把这条拒了 —— 那正是用户报的「同时只能运行一个会话」');

      final s = c.read(chatControllerProvider);
      expect(s.sendError, isNull, reason: '收下了就不该留一条错误横幅');
      final b = s.activeSessionId!;
      expect(b, isNot(a), reason: '白纸该兑现成另一条会话');
      expect(s.streamingTurns.keys.toSet(), {a, b}, reason: '两条会话该各有各的那一轮在跑');

      // ⚠️ **这条不变量不许跟着一起松掉。**
      // 并跑之后每条流各写各的转录，串台的表现是一句话出现在别人的对话里
      expect(
        (s.transcripts[a]?.messages ?? const []).where(
          (m) => m.text.contains('白纸上这条'),
        ),
        isEmpty,
        reason: 'B 的消息落进了 A 的转录 —— 并跑之后这是最容易犯的错',
      );
    });

    /// **同一条会话仍然不许并跑，而且拒收必须出声。**
    ///
    /// 那条不变量没变：两轮往同一段历史里追加消息、写同一个工作区的文件，
    /// 谁覆盖谁取决于时序。而「拒收必须出声」是 2026-08-29 那次的收口 ——
    /// 静默 return 的表现是「发都发不出去，直接静默消失」。
    test('同一条会话里再发一条，拒收且出声', () async {
      final c = _boot();
      addTearDown(c.dispose);
      await _settle(c);

      final ctrl = c.read(chatControllerProvider.notifier);
      final a = ctrl.createSession();
      expect(await ctrl.send('第一条，占住流'), isTrue);
      expect(c.read(chatControllerProvider).streamingTurns[a], isNotNull);

      // 不切走，就在这条会话里再发
      final second = await ctrl.send('同一条会话里的第二句');
      expect(second, isFalse, reason: '同一条会话并跑两轮会让两轮互相覆盖');
      expect(
        c.read(chatControllerProvider).sendError,
        isNotNull,
        reason: '拒收必须出声。静默 return 正是「消息凭空消失」的那半',
      );
    });
  });

  group('composer 在拒收时把文字还回输入框', () {
    testWidgets('onSend 回 false → 文本回来；回 true → 保持清空', (tester) async {
      // ⚠️ 不用裸 ProviderScope：那会把真的 auth bootstrap 拉起来，
      // rememberedTokenProvider 留一个 3 秒 timer，CI 上稳定红在
      // 「A Timer is still pending」（本机因 secure storage 插件缺失时序
      // 不同，恰好绿 —— 别信本机）。与 blind_model_image_gate_test 同款：
      // mock 配置的容器。
      for (final (accepted, expectAfter) in [(false, '要还回来的话'), (true, '')]) {
        final c = _boot();
        addTearDown(c.dispose);
        await tester.pumpWidget(
          UncontrolledProviderScope(
            container: c,
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
        // mock 数据源带仿真延迟且一环扣一环（列表→详情→…），单补一次
        // pump 是打地鼠。把假时钟连推若干拍直到它们全走完 —— 不然测试
        // 结束时还挂着 pending timer，红在与本测试无关的一句断言上
        for (var i = 0; i < 8; i++) {
          await tester.pump(const Duration(milliseconds: 400));
        }
        // 主动卸载这一轮的树并补两拍：riverpod 在卸载时排一个 0ms 的
        // dispose 任务，不给它一拍就悬在测试结束的那道 timer 断言上
        await tester.pumpWidget(const SizedBox.shrink());
        await tester.pump();
        await tester.pump();
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
