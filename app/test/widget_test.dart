import 'package:cortex_app/app.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/memory_drawer.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

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
  Future<void> boot(WidgetTester tester, {Size size = const Size(1600, 1000)}) async {
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
    expect(find.byType(MemoryDrawer), findsOneWidget);

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
}
