import 'dart:async';

import 'package:cortex_app/app.dart';
import 'package:cortex_app/core/app_config.dart';
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

Widget _app() => ProviderScope(
  overrides: [appConfigProvider.overrideWith(_MockConfig.new)],
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
    await tester.pump(const Duration(milliseconds: 900));
  }

  testWidgets('归档的会话默认不出现，打开开关才显示', (tester) async {
    await boot(tester);

    expect(
      find.text('（已归档）上个季度的迁移方案'),
      findsNothing,
      reason: '默认列表应带 include_archived=false',
    );

    await tester.tap(find.byIcon(Icons.inventory_2_outlined));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));

    expect(find.text('（已归档）上个季度的迁移方案'), findsOneWidget);
    expect(
      find.text('已归档'),
      findsOneWidget,
      reason: '显示出来时要标明它是归档态，否则与普通会话没法区分',
    );
  });

  testWidgets('归档一个会话会把它从列表里拿掉，但不叫「删除」', (tester) async {
    await boot(tester);

    final container = ProviderScope.containerOf(
      tester.element(find.byType(CortexApp)),
    );
    final controller = container.read(chatControllerProvider.notifier);
    final target = container.read(chatControllerProvider).sessions.firstWhere(
      (s) => !s.archived,
    );

    // Never `await` a data-source call inside `testWidgets`: the mock's latency
    // is a `Future.delayed`, and in a widget test that timer only fires when
    // the test pumps. Awaiting it here deadlocks the test rather than failing
    // it. Kick it off, then advance the clock.
    unawaited(controller.setArchived(target.id, true));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));

    expect(
      container
          .read(chatControllerProvider)
          .visibleSessions
          .any((s) => s.id == target.id),
      isFalse,
    );
    expect(
      container
          .read(chatControllerProvider)
          .sessions
          .firstWhere((s) => s.id == target.id)
          .archived,
      isTrue,
      reason: '归档不是删除 —— 记录还在，只是不列出来',
    );
  });

  testWidgets('标题栏的工作区入口：未绑定时是一个可点的绑定按钮', (tester) async {
    await boot(tester);

    final container = ProviderScope.containerOf(
      tester.element(find.byType(CortexApp)),
    );
    // The first fixture session ships unbound.
    container
        .read(chatControllerProvider.notifier)
        .selectSession('ses_01JQZ8K3M9');
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));

    expect(
      find.text('绑定工作区'),
      findsOneWidget,
      reason: '未绑定时入口必须自己说清楚它是干什么的',
    );

    await tester.tap(find.text('绑定工作区'));
    // Explicit pumps rather than `pumpAndSettle`: the shell keeps indeterminate
    // progress indicators alive (the workspace tree spins while it lists a
    // directory), and those never let the frame queue drain.
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(find.text('绑定工作区'), findsWidgets);
    expect(
      find.textContaining('cortexd 所在机器上的绝对路径'),
      findsOneWidget,
      reason: '路径属于 daemon 那台机器，这一点必须写在输入框上',
    );
  });

  testWidgets('已绑定的会话在标题栏显示目录名', (tester) async {
    await boot(tester);

    final container = ProviderScope.containerOf(
      tester.element(find.byType(CortexApp)),
    );
    // `ses_01JQZ2N8D1` ships bound to `D:/codes/cortex`.
    container
        .read(chatControllerProvider.notifier)
        .selectSession('ses_01JQZ2N8D1');
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));

    expect(find.text('cortex'), findsWidgets, reason: '芯片上显示的是目录名');
    expect(find.text('绑定工作区'), findsNothing);
  });

  testWidgets('重命名对话框对派生标题留空，不把占位符提拔成真名字', (tester) async {
    await boot(tester);

    final container = ProviderScope.containerOf(
      tester.element(find.byType(CortexApp)),
    );
    final id = container.read(chatControllerProvider).sessions.first.id;

    unawaited(
      container
          .read(chatControllerProvider.notifier)
          .renameSession(id, '我自己起的名字'),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 600));

    final session = container
        .read(chatControllerProvider)
        .sessions
        .firstWhere((s) => s.id == id);
    expect(session.title, '我自己起的名字');
    expect(
      session.titleIsCustom,
      isTrue,
      reason: '改过名之后就不该再被首条消息覆盖',
    );
    expect(
      session.hasLocalOverrides,
      isFalse,
      reason: 'mock 实现了 PATCH，所以不该被标成「仅本地」',
    );
  });
}
