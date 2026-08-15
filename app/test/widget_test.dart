import 'package:cortex_app/app.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/conversation_view.dart';
import 'package:cortex_app/features/chat/widgets/turn_drawer.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/chat_state.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/replay_api.dart';

/// Points the app at a "live" backend so an injected fake is used instead of
/// `MockCortexApi`. Nothing reaches the network — `cortexApiProvider` is
/// overridden alongside it.
class _LiveConfigNotifier extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:8080');
}

/// Pins the app to the mock source regardless of `--dart-define`, so tests are
/// hermetic and never touch the network.
class _MockConfigNotifier extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

Widget _app() => ProviderScope(
  overrides: [appConfigProvider.overrideWith(_MockConfigNotifier.new)],
  child: const CortexApp(),
);

void main() {
  Future<void> boot(
    WidgetTester tester, {
    Size size = const Size(1600, 1000),
  }) async {
    tester.view.physicalSize = size;
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(_app());
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 800));
  }

  testWidgets('wide layout shows the session pane and the chat pane', (
    tester,
  ) async {
    await boot(tester);

    // 右栏默认收起 —— 记忆那一格去了 Cormex，剩下的文件面板在没绑工作区时
    // 是空的，默认展开一个空面板只是白占三分之一屏。
    expect(find.text('会话'), findsOneWidget);
    // Appears twice by design: the sidebar tile and the chat pane header, since
    // the first session is auto-selected on load.
    expect(find.text('Cortex 记忆注入预算怎么定'), findsNWidgets(2));
  });

  testWidgets('narrow layout collapses the side panes into drawers', (
    tester,
  ) async {
    await boot(tester, size: const Size(700, 1000));

    // Session pane is no longer resident; the hamburger takes its place.
    expect(find.text('会话'), findsNothing);
    expect(find.byIcon(Icons.menu_rounded), findsOneWidget);
  });

  // 「记忆面板的 as_of 时间回放」与「检索空态」两条用例随记忆界面一起去了
  // Cormex —— 它们测的是那个面板，而这个客户端不再有它。
  //
  // 那两条都是好测试（时间回放是这套东西的差异点，空态不能读成错误），
  // 所以它们不是被删掉，是**跟着被测对象搬走了**：Cormex 的 app/ 里有对应覆盖。

  testWidgets('streams a reply and exposes the per-turn memory', (
    tester,
  ) async {
    await boot(tester);

    final controller = ProviderScope.containerOf(
      tester.element(find.byType(CortexApp)),
    ).read(chatControllerProvider.notifier);

    controller.send('Rust async trait 怎么选');
    await tester.pump();

    // Mock holds back ~420ms before the first tool event, mirroring retrieval.
    await tester.pump(const Duration(milliseconds: 500));

    expect(find.byType(TurnDrawer), findsAtLeastNWidgets(1));

    // Partway through the stream there must be text on screen but the turn is
    // still in flight — a client that awaited the whole body would show
    // nothing here.
    await tester.pump(const Duration(milliseconds: 400));
    final container = ProviderScope.containerOf(
      tester.element(find.byType(CortexApp)),
    );
    final mid = container.read(chatControllerProvider);
    expect(mid.streaming, isNotNull);
    expect(mid.streaming!.text.isNotEmpty, isTrue);

    final midLength = mid.streaming!.text.length;

    // Deltas append — length only ever grows.
    await tester.pump(const Duration(milliseconds: 400));
    final later = container.read(chatControllerProvider);
    if (later.streaming != null) {
      expect(later.streaming!.text.length, greaterThanOrEqualTo(midLength));
      expect(later.streaming!.text.startsWith(mid.streaming!.text), isTrue);
    }

    // Drain the remaining deltas so no timer outlives the test.
    for (var i = 0; i < 600; i++) {
      await tester.pump(const Duration(milliseconds: 30));
    }
  });

  testWidgets('滚到顶部的按钮真的会去拉上一页', (tester) async {
    tester.view.physicalSize = const Size(900, 1200);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    final api = ReplayApi(episodeCount: 130);
    await tester.pumpWidget(
      ProviderScope(
        overrides: [
          appConfigProvider.overrideWith(_LiveConfigNotifier.new),
          cortexApiProvider.overrideWithValue(api),
        ],
        child: const MaterialApp(home: Scaffold(body: ConversationView())),
      ),
    );
    // Not `await`ing the source: the futures resolve on microtask drain, and
    // awaiting one from inside `testWidgets` is how this suite deadlocks.
    await tester.pump();
    await tester.pump();
    await tester.pump();

    // The view opens pinned to the newest message, so the header at index 0 is
    // off-screen and therefore not built — scroll up to it the way a user does.
    final loadMore = find.text('加载更早的 $kEpisodePage 条');
    await tester.scrollUntilVisible(
      loadMore,
      -300,
      scrollable: find.byType(Scrollable).first,
    );
    expect(loadMore, findsOneWidget, reason: '服务端说了 has_more，入口就必须在');
    expect(
      find.textContaining('最新几轮可能不在其中'),
      findsNothing,
      reason: '升序截断的横幅是给旧服务端的止血措施，服务端修好后必须撤掉',
    );

    await tester.tap(loadMore);
    await tester.pump();
    await tester.pump();
    await tester.pump();

    expect(
      api.detailCalls,
      hasLength(2),
      reason: '这个按钮是一次真正的取数，不再是「展开已经在内存里的东西」',
    );
    expect(api.detailCalls.last.$3, isNotNull, reason: '第二次必须带游标');
  });
}
