/// **切回一个旧会话时，滚动条要在最底下。**
///
/// 2026-08-30 实报：「每次切换对话后再回去，滚动条会自动定位到靠上的位置，
/// 而不是在最下面」。
///
/// 成因不在「有没有滚到底」那行代码上 —— 它一直在。成因是**只跳了一帧**：
/// 这是个懒构建、变高条目的 `ListView.builder`，没排过版的条目它不知道多高，
/// 于是 `maxScrollExtent` 是按已排版部分外推的**估算值**。切换会话那一刻
/// 只排了一屏，估算远小于真实高度 —— 跳过去、更多条目跟着排版、extent 变大，
/// 位置留在原地，看起来就是停在靠上的地方。
///
/// # 为什么必须造一个「切走再切回」的用例
///
/// **首次打开时它蒙对了**：消息是异步到的，`activeTranscript.last.id` 一变，
/// 那条 force 监听会再跳几次，正好把位置带到底。而切**回**一个已经缓存的会话
/// 时最新消息没变，那条监听根本不响 —— 只剩会话切换这一条跳一次。
///
/// 所以一条「打开会话看看在不在底部」的测试是**绿的**，而 bug 照旧存在。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/conversation_view.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/models/episode.dart';
import 'package:cortex_app/models/session_detail.dart';
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

ChatSession _session(String id) =>
    ChatSession(id: id, title: '会话 $id', updatedAt: DateTime.utc(2026, 8, 30));

/// 两个都装不下一屏的会话。**条目要变高**（长短交替）—— 等高的话
/// `maxScrollExtent` 的估算恰好是准的，这个 bug 根本复现不出来。
class _LongApi extends MockCortexApi {
  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => [_session('a'), _session('b')];

  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async => SessionDetail(
    session: _session(id),
    episodes: List.generate(40, (i) {
      // ★ **前面短、后面长** —— 这是真实会话的形状，也是唯一能复现的形状。
      //
      // `ListView` 按**已排版部分的均值**外推 `maxScrollExtent`。条目等高、
      // 或者长短均匀交替时，从第一屏推出来的均值恰好是对的，估算就是准的 ——
      // 那样写出来的测试撤掉修复照样绿（第一版就是这么写的，当场验出来是空的）。
      //
      // 前 30 条很短、后 10 条极长时，从第一屏（全是短的）外推出的高度**远小于**
      // 真实高度，那一跳就会停在半路 —— 用户看到的正是这个。
      final body = i < 30 ? '短消息' : '很长的一段回答。' * 120;
      return Episode(
        id: '$id-ep-$i',
        sessionId: id,
        role: i.isEven ? 'user' : 'assistant',
        text: '$i · $body',
        occurredAt: DateTime.utc(2026, 8, 30, 0, i),
      );
    }),
  );
}

void main() {
  testWidgets('切走再切回，滚动条仍然在最底下', (tester) async {
    final container = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWith((ref) => _LongApi()),
      ],
    );
    addTearDown(container.dispose);

    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(
          home: Scaffold(
            body: SizedBox(height: 400, width: 600, child: ConversationView()),
          ),
        ),
      ),
    );

    final notifier = container.read(chatControllerProvider.notifier);
    Future<void> settle() async {
      for (var i = 0; i < 30; i++) {
        await tester.pump(const Duration(milliseconds: 16));
      }
    }

    await settle();

    ScrollPosition position() =>
        tester.state<ScrollableState>(find.byType(Scrollable).first).position;

    /// 距离底部还有多远。0 = 贴底。
    double gap() => position().maxScrollExtent - position().pixels;

    notifier.selectSession('a');
    await settle();
    expect(
      position().maxScrollExtent,
      greaterThan(0),
      reason: '前提没成立：这份会话没有超出一屏，测不出滚动位置的问题',
    );
    expect(gap(), lessThan(2), reason: '第一次打开就没到底 —— 那是另一个问题，先修那个');

    // 切走
    notifier.selectSession('b');
    await settle();

    // ★ **切回来。** 这时 transcript 已经缓存着，最新消息没变，
    // 于是按最新消息触发的那条 force 监听不会响 —— bug 就活在这里。
    notifier.selectSession('a');
    await settle();

    expect(
      gap(),
      lessThan(2),
      reason:
          '切回旧会话之后停在了距底 ${gap().toStringAsFixed(0)} 像素的地方。'
          'ListView 是懒构建的，maxScrollExtent 在只排了一屏时是个远小于真实'
          '高度的估算值 —— 只跳一帧就会停在半路，而用户看到的正是'
          '「滚动条自动定位到靠上的位置」',
    );
  });
}
