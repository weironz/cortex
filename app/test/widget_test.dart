import 'package:cortex_app/app.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/conversation_view.dart';
import 'package:cortex_app/features/chat/widgets/memory_drawer.dart';
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

  testWidgets('wide layout shows all three panes', (tester) async {
    await boot(tester);

    expect(find.text('会话'), findsOneWidget);
    expect(find.text('记忆'), findsOneWidget);
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

  testWidgets('memory panel exposes the as_of time-travel control', (
    tester,
  ) async {
    await boot(tester);

    // Default state is "now".
    expect(find.text('当前时刻的记忆'), findsOneWidget);

    await tester.tap(find.text('当前时刻的记忆'));
    await tester.pumpAndSettle();

    // A date picker opens; pick whatever day is preselected and confirm.
    expect(find.text('回放到哪一天为止已知的记忆'), findsOneWidget);
    await tester.tap(find.text('OK'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));

    // The bar switches to the replay state and offers a way back.
    expect(find.textContaining('回放至'), findsOneWidget);
    expect(find.byTooltip('回到现在'), findsOneWidget);

    await tester.tap(find.byTooltip('回到现在'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));
    expect(find.text('当前时刻的记忆'), findsOneWidget);
  });

  testWidgets('检索无结果时是中性空态，不是错误', (tester) async {
    await boot(tester);

    await tester.enterText(
      find.widgetWithText(TextField, '检索事实、实体、领域…'),
      'zzz 不存在的东西 zzz',
    );
    // Debounce (260ms) plus the mock's simulated retrieval latency.
    await tester.pump(const Duration(milliseconds: 400));
    await tester.pump(const Duration(milliseconds: 600));

    expect(find.text('没有相关的记忆'), findsOneWidget);
    expect(
      find.textContaining('主动弃权'),
      findsOneWidget,
      reason: '空结果必须解释成检索器弃权，而不是让用户以为坏了',
    );
    expect(
      find.text('检索失败'),
      findsNothing,
      reason: '空不是错，出现错误标题就是在训练用户不信任正确结果',
    );
  });

  testWidgets('streams a reply and exposes the per-turn memory', (
    tester,
  ) async {
    await boot(tester);

    final controller = ProviderScope.containerOf(
      tester.element(find.byType(CortexApp)),
    ).read(chatControllerProvider.notifier);

    controller.send('Rust async trait 怎么选');
    await tester.pump();

    // Mock holds back ~420ms before the memory event, mirroring retrieval.
    await tester.pump(const Duration(milliseconds: 500));

    expect(find.byType(MemoryDrawer), findsAtLeastNWidgets(1));

    // Two drawers, and both matter. The live turn has always had one; the
    // replayed turn above it only got one when `episode_memories` /
    // `episode_tool_calls` landed. Before that, the same answer looked
    // different before and after a refresh.
    expect(
      find.textContaining('本轮用到的记忆'),
      findsNWidgets(2),
      reason: '回放出来的历史轮次与流式中的当前轮次都应带记忆抽屉',
    );

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
    expect(mid.streaming!.facts, isNotEmpty);

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
