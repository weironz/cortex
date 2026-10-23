/// 输入框底下那条**用量状态栏**，以及喂给它的那份账。
///
/// 盯住四件事：
///
/// 1. **没报的项不画** —— 供应商不说缓存，界面上就没有「缓存 0%」那一句。
///    0 与「不知道」被画成同一个样子，是这条状态栏最容易犯的错
/// 2. **子 agent 的用量进总数、不进上下文** —— 混进去的话那个百分比会随着
///    四路并行来回跳，而它本来说的是「你这条对话还能装多少」
/// 3. **速度的分母只算真的在吐字的那段**，量不出耗时就不画
/// 4. **算出来的数字真的画在屏幕上** —— 「值算对了却没画出来」是这个仓库
///    咬过人的形状
///
/// 与 [usage_test.dart] 不同：那个管的是设置页的**花费**（¥ 与配额），
/// 这个管的是这一轮的 **token**。两件事的判据不一样，不要合并。
library;

import 'dart:async';

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/features/chat/widgets/usage_status_bar.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/models/turn_usage.dart';
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

/// 一轮的每个事件由测试自己往里放。
class _ScriptedApi extends MockCortexApi {
  final _controllers = <StreamController<ChatEvent>>[];

  StreamController<ChatEvent> get current => _controllers.last;

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    Assistant? assistant,
    List<Skill> skills = const [],
    bool computerUse = false,
    ImagePrefs? imagePrefs,
  }) {
    final c = StreamController<ChatEvent>();
    _controllers.add(c);
    return c.stream;
  }
}

ProviderContainer _boot(MockCortexApi api) {
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_MockConfig.new),
      cortexApiProvider.overrideWithValue(api),
    ],
  );
  container.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  return container;
}

Future<void> _until(
  bool Function() cond, {
  String reason = '',
  Duration timeout = const Duration(seconds: 15),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!cond()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

void main() {
  group('用量事件的解码', () {
    test('字段全的一个都不丢', () {
      final ev = ChatEvent.fromJson(const {
        'type': 'usage',
        'model': 'qwen-flash',
        'input_tokens': 1200,
        'output_tokens': 240,
        'cache_read_input_tokens': 1024,
        'cache_write_input_tokens': 12,
        'generation_ms': 320,
      });
      expect(ev, isA<ChatUsageEvent>(), reason: '不认识的话会落到 ChatUnknownEvent');
      final u = ev as ChatUsageEvent;
      expect(u.model, 'qwen-flash');
      expect(u.inputTokens, 1200);
      expect(u.outputTokens, 240);
      expect(u.cacheReadInputTokens, 1024);
      expect(u.cacheWriteInputTokens, 12);
      expect(u.generationMs, 320);
      expect(
        u.tokensPerSecond,
        closeTo(750, 0.001),
        reason: '240 个 token ÷ 0.32 秒 —— 这个数就是状态栏上那个 750 tok/s',
      );
    });

    test('没报的字段是 null，不是 0', () {
      final ev =
          ChatEvent.fromJson(const {'type': 'usage', 'generation_ms': 0})
              as ChatUsageEvent;
      expect(ev.inputTokens, isNull);
      expect(ev.outputTokens, isNull);
      expect(ev.cacheReadInputTokens, isNull);
      expect(ev.model, isNull, reason: '模型名没报时不能编一个默认模型');
      expect(
        ev.tokensPerSecond,
        isNull,
        reason: '0 毫秒 = 量不出来；画成 0 tok/s 是在说另一句话',
      );
    });

    test('负数 token 当没报', () {
      final ev =
          ChatEvent.fromJson(const {
                'type': 'usage',
                'input_tokens': -1,
                'output_tokens': -1,
              })
              as ChatUsageEvent;
      expect(ev.inputTokens, isNull, reason: '有的端点拿 -1 表示「没有这一项」');
      expect(ev.outputTokens, isNull);
    });
  });

  group('账本', () {
    const first = ChatUsageEvent(
      model: 'qwen-flash',
      inputTokens: 360000,
      outputTokens: 250,
      cacheReadInputTokens: 324000,
      generationMs: 500,
    );

    test('累加：总数、速度、缓存命中率', () {
      final u = const TurnUsage().add(first);
      expect(u.rounds, 1);
      expect(u.input, 360000);
      expect(u.output, 250);
      expect(u.tokensPerSecond, 500, reason: '250 token ÷ 0.5 秒');
      expect(
        u.cacheHitRate,
        closeTo(0.9, 1e-9),
        reason: '缓存命中是输入的子集，分母是输入不是输入加输出',
      );
      expect(u.lastInput, 360000);
      expect(u.lastModel, 'qwen-flash');
      expect(u.isEmpty, isFalse);
    });

    test('量不出耗时的段不进分母', () {
      final u = const TurnUsage()
          .add(first)
          // 这一段只报了 token，没报耗时（模型只调了工具）
          .add(const ChatUsageEvent(inputTokens: 100, outputTokens: 900));
      expect(u.output, 1150, reason: '总数要算上它');
      expect(u.tokensPerSecond, 500, reason: '900 个 token 没有耗时，算进分母会把速度稀释成假的');
    });

    test('缓存没报就不给命中率', () {
      final u = const TurnUsage().add(
        const ChatUsageEvent(inputTokens: 1200, outputTokens: 30),
      );
      expect(u.cacheHitRate, isNull, reason: '「一次都没命中」与「这家供应商不说」不能画成同一个 0%');
    });

    test('报了 0 就画 0%', () {
      final u = const TurnUsage().add(
        const ChatUsageEvent(
          inputTokens: 1200,
          outputTokens: 30,
          cacheReadInputTokens: 0,
        ),
      );
      expect(
        u.cacheHitRate,
        0.0,
        reason: '供应商如实报了「一个都没命中」—— 那是事实，不画等于把它说成「不知道」',
      );
    });

    test('只报缓存不报输入时不画出超过 100% 的命中率', () {
      final u = const TurnUsage()
          .add(
            const ChatUsageEvent(inputTokens: 100, cacheReadInputTokens: 800),
          )
          .add(const ChatUsageEvent(cacheReadInputTokens: 900));
      expect(
        u.cacheHitRate,
        1.0,
        reason: '两边口径对不上时比例会超过 1，那是「这个数不可信」，不是命中 800%',
      );
    });

    test('子 agent 的用量进总数，但不进上下文', () {
      final u = const TurnUsage()
          .add(first)
          .add(
            const ChatUsageEvent(
              model: 'qwen-flash',
              inputTokens: 900,
              outputTokens: 100,
              generationMs: 100,
              subagent: SubagentTag(index: 1, task: '查一下'),
            ),
          );
      expect(u.input, 360900, reason: '子 agent 烧的 token 也是钱，要算进去');
      expect(u.output, 350);
      expect(u.lastInput, 360000, reason: '子 agent 的提示词是一小段任务，混进来会让上下文百分比来回跳');
      expect(
        u.tokensPerSecond,
        500,
        reason:
            '速度只算主 agent 那一段：子 agent 是并行跑的，四路的生成时间互相'
            '重叠 —— 加进分母等于把同一段时间算两遍，而界面看的是主 agent 在吐字',
      );
    });

    test('学到的字/token 比只用于轮内估算', () {
      // 这一段吐了 500 个字、报了 250 个 token
      final u = const TurnUsage().add(first, chars: 500);
      expect(u.charsPerToken, 2.0);
    });
  });

  group('格式化', () {
    test('token 数压成量级', () {
      expect(formatCompactTokens(999), '999');
      expect(formatCompactTokens(1200), '1.2k');
      expect(formatCompactTokens(93500000), '93.5M');
    });

    test('不知道窗口就不画百分比', () {
      expect(contextPercent(360000, null), isNull);
      expect(contextPercent(360000, 0), isNull);
      expect(contextPercent(null, 200000), isNull);
      expect(contextPercent(360000, 1000000), 36);
    });
  });

  group('控制器', () {
    test('流里的用量入账，收尾之后还留在会话上', () async {
      final api = _ScriptedApi();
      final container = _boot(api);
      addTearDown(container.dispose);
      final ctrl = container.read(chatControllerProvider.notifier);
      await _until(
        () => !container.read(chatControllerProvider).sessionsLoading,
        reason: '会话列表',
      );

      final sid = ctrl.createSession();
      await ctrl.send('跑一轮');
      await _until(
        () =>
            container.read(chatControllerProvider).streamingTurns[sid] != null,
        reason: '那一轮开始',
      );

      api.current
        ..add(const ChatDeltaEvent('一二三四五'))
        ..add(
          const ChatUsageEvent(
            model: 'qwen-flash',
            inputTokens: 360000,
            outputTokens: 250,
            cacheReadInputTokens: 324000,
            generationMs: 500,
          ),
        );
      await _until(
        () =>
            container
                .read(chatControllerProvider)
                .streamingTurns[sid]!
                .usage
                .rounds ==
            1,
        reason: '用量入账',
      );
      final live = container.read(chatControllerProvider).streamingTurns[sid]!;
      expect(live.usage.tokensPerSecond, 500);
      expect(
        live.usage.charsPerToken,
        closeTo(0.02, 1e-9),
        reason: '5 个字 ÷ 250 个 token —— 轮内估算就靠这个比',
      );

      api.current
        ..add(const ChatDoneEvent('ep'))
        ..close();
      await _until(
        () =>
            container.read(chatControllerProvider).streamingTurns[sid] == null,
        reason: '收尾',
      );

      final saved = container.read(chatControllerProvider).turnUsages[sid];
      expect(saved, isNotNull, reason: '气泡收起来之后状态栏还在，它读的是会话上这一份');
      expect(saved!.output, 250);
    });
  });

  testWidgets('算出来的数字真的画在屏幕上', (tester) async {
    final container = _boot(MockCortexApi(instant: true));
    addTearDown(container.dispose);
    final ctrl = container.read(chatControllerProvider.notifier);

    for (
      var i = 0;
      i < 100 && container.read(chatControllerProvider).sessionsLoading;
      i++
    ) {
      await tester.pump(const Duration(milliseconds: 10));
    }
    final sid = ctrl.createSession();
    unawaited(ctrl.send('你好'));
    // mock 那条流按字吐，每滴之间有几毫秒的假延迟 —— 推着假时钟走完它
    for (
      var i = 0;
      i < 600 && container.read(chatControllerProvider).turnUsages[sid] == null;
      i++
    ) {
      await tester.pump(const Duration(milliseconds: 20));
    }
    expect(
      container.read(chatControllerProvider).turnUsages[sid],
      isNotNull,
      reason: '这一轮没跑完 —— 后面的断言就是在对着空白画布说话',
    );

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: MaterialApp(
          home: Scaffold(body: UsageStatusBar(sessionId: sid)),
        ),
      ),
    );
    await tester.pump();

    expect(
      find.textContaining('tok/s'),
      findsOneWidget,
      reason: '速度是这个功能的主题，画不出来等于没做',
    );
    expect(
      find.textContaining('上下文'),
      findsOneWidget,
      reason: '上下文百分比是「还能聊多久」的答案 —— 模型窗口查得到就该画',
    );
    expect(find.textContaining('↑'), findsOneWidget, reason: '输入输出 token 数');
  });
}
