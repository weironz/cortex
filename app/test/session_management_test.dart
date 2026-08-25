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
    final target = container
        .read(chatControllerProvider)
        .sessions
        .firstWhere((s) => !s.archived);

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

  /// 入口从标题栏搬到了输入框底下（与权限档并排）：那两件事是同一类 ——
  /// 发出去**之前**要定的。标题栏那一排剩下的都是应用级的显示开关。
  ///
  /// 文案也跟着变了。夹具里这条会话已经聊过、又没有本机绑定，那它就是
  /// **跑在云端**的 —— chip 照实说「云端」。上一版在这里写「绑定工作区」，
  /// 那既没说清它现在在哪儿跑，也没说清点下去会得到什么。
  ///
  /// （「新建工作区」是**草稿**才有的那一档：只有还没开口的会话，才谈得上
  /// 「发出第一句话时给你开一个」。）
  testWidgets('输入框底下的工作区入口：没绑本机的会话说它已经派出去了', (tester) async {
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
      find.text('已派出去'),
      findsOneWidget,
      reason:
          '这条会话没有本机绑定，那它的每一轮就是在远端容器里跑的。'
          '入口要照实说它现在在哪儿 —— 说「未绑定」只描述了缺什么，'
          '没回答用户真正要问的「我的文件在哪」。'
          '而它叫「已派出去」不叫「云端」：后者是一个存储位置的名字，'
          '于是这个能力没有名字，只能被碰巧发现',
    );

    await tester.tap(find.text('已派出去'));
    // Explicit pumps rather than `pumpAndSettle`: the shell keeps indeterminate
    // progress indicators alive (the workspace tree spins while it lists a
    // directory), and those never let the frame queue drain.
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    // 点开的是选择器，不再是直接弹绑定框：「云端」与「本机某个目录」
    // 现在是同一个清单上的两项
    expect(
      find.text('选择其他文件夹…'),
      findsOneWidget,
      reason: '默认工作空间之外的任意目录仍然要有一条路进得去',
    );
    expect(
      find.text('派出去跑'),
      findsOneWidget,
      reason:
          '清单里那一条与 chip 上的标签必须是同一个名字 —— 不同名的话，'
          '用户在清单里选了「派出去跑」，回来看见 chip 写着别的，会以为点错了',
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
    expect(session.titleIsCustom, isTrue, reason: '改过名之后就不该再被首条消息覆盖');
    expect(
      session.hasLocalOverrides,
      isFalse,
      reason: 'mock 实现了 PATCH，所以不该被标成「仅本地」',
    );
  });
}
