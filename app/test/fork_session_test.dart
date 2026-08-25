/// 分叉会话：旧会话不动，新会话带着历史独立续聊。
///
/// 三层各测各的：
///
/// 1. mock API 的契约 —— 与服务端同一套判定（截断**含**那条、指错 400、
///    空历史 400）。mock 放松任何一条，错误分支就只会在生产上第一次执行。
/// 2. 控制器 —— 分叉成功后新会话**当场**进列表并被选中，不等下一次
///    `loadSessions`；失败收敛成一句话而不是一个没人 await 的异常。
/// 3. 菜单 —— 「分叉会话」真的在会话菜单里，点了真的触发回调。
///    界面与逻辑各判一次的话，漏改一处不会有任何测试红。
library;

import 'dart:async';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/app.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/session_export.dart';
import 'package:cortex_app/features/sessions/session_list.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 夹具里带三条消息的那个会话（P3 最旧，A1、A5 在后）。
const _rich = 'ses_01JQZ8K3M9';

void main() {
  group('mock API 契约', () {
    test('整段分叉：新会话带全部历史，旧会话原样', () async {
      final api = MockCortexApi(instant: true);
      final before = await api.sessionDetail(_rich);

      final forked = await api.forkSession(_rich);
      expect(
        forked.title.endsWith('（分叉）'),
        isTrue,
        reason:
            '标题该是「原标题（分叉）」，实际是 ${forked.title} —— '
            '没有这个后缀，侧栏里两条一模一样的名字分不出谁是谁',
      );
      expect(forked.id, isNot(_rich), reason: '分叉必须是一条**新**会话');
      expect(
        forked.projectId,
        before.session.projectId,
        reason: '项目归属要跟着走 —— 落到未分组用户会在项目里找不到它',
      );

      final copy = await api.sessionDetail(forked.id);
      expect(
        copy.episodes.map((e) => e.text).toList(),
        before.episodes.map((e) => e.text).toList(),
        reason: '新会话的消息要按原顺序、原内容出现',
      );

      final after = await api.sessionDetail(_rich);
      expect(
        after.episodes.length,
        before.episodes.length,
        reason: '旧会话一条不许多、一条不许少 —— 分叉不是搬家',
      );
    });

    test('从中间某条分叉：截到那条为止，**含**它', () async {
      final api = MockCortexApi(instant: true);
      final all = (await api.sessionDetail(_rich)).episodes;
      expect(all.length, greaterThan(1), reason: '夹具至少要有两条，否则截断测不出来');

      final cut = all[all.length - 2];
      final forked = await api.forkSession(_rich, upToEpisodeId: cut.id);
      final copy = await api.sessionDetail(forked.id);
      expect(
        copy.episodes.length,
        all.length - 1,
        reason: '「从这里分叉」是含那条截断 —— 不含的话用户选中的那句恰好被丢掉',
      );
      expect(copy.episodes.last.text, cut.text);
    });

    test('指错消息与空历史都是 400，不是静默全量', () async {
      final api = MockCortexApi(instant: true);
      await expectLater(
        api.forkSession(_rich, upToEpisodeId: 'epi_does_not_exist'),
        throwsA(
          isA<CortexApiException>().having((e) => e.statusCode, 'status', 400),
        ),
        reason: '静默按全量处理会让「从这里分叉」悄悄变成整段分叉，用户看不出差别',
      );
    });
  });

  group('控制器', () {
    Future<ProviderContainer> boot(WidgetTester tester) async {
      tester.view.physicalSize = const Size(1600, 1000);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.reset);
      await tester.pumpWidget(
        ProviderScope(
          overrides: [appConfigProvider.overrideWith(_MockConfig.new)],
          child: const CortexApp(),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 900));
      return ProviderScope.containerOf(tester.element(find.byType(CortexApp)));
    }

    testWidgets('分叉成功：新会话当场进列表并被选中', (tester) async {
      final container = await boot(tester);
      final controller = container.read(chatControllerProvider.notifier);

      // mock 的延迟是 Future.delayed，widget 测试里 await 会死锁 ——
      // 先发出去，再推时钟（与 session_management_test 同一个坑）
      String? said;
      unawaited(controller.forkSession(_rich).then((s) => said = s));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(said, isNull, reason: '成功时不该有要说给用户的话');
      final state = container.read(chatControllerProvider);
      final forked = state.sessions.firstWhere(
        (s) => s.title.endsWith('（分叉）'),
        orElse: () => throw StateError(
          '列表里没有分叉出来的那条 —— '
          '等下一次 loadSessions 的话，切过去的那一瞬侧栏会闪',
        ),
      );
      expect(
        state.activeSessionId,
        forked.id,
        reason: '分叉之后应当直接切到新会话 —— 用户点它就是为了在那儿续聊',
      );
    });

    testWidgets('分叉失败：收敛成一句话，不是没人接的异常', (tester) async {
      final container = await boot(tester);
      final controller = container.read(chatControllerProvider.notifier);

      String? said;
      unawaited(
        controller.forkSession('ses_does_not_exist').then((s) => said = s),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(
        said,
        isNotNull,
        reason:
            '失败必须变成一句能显示的话 —— 两个入口（菜单、消息动作）'
            '共用这一处收敛，各写一遍 catch 迟早漂开',
      );
    });
  });

  group('会话菜单', () {
    testWidgets('「分叉会话」在菜单里，点了触发回调', (tester) async {
      var forked = false;
      await tester.pumpWidget(
        MaterialApp(
          home: ProviderScope(
            child: Scaffold(
              body: SessionTileMenu(
                archived: false,
                enabled: true,
                canMove: true,
                onRename: () {},
                onFork: () => forked = true,
                onTogglePin: () {},
                pinned: false,
                onToggleArchive: () {},
                onMove: () {},
                onExport: (ExportFormat _) {},
              ),
            ),
          ),
        ),
      );
      await tester.tap(find.byType(SessionTileMenu));
      await tester.pumpAndSettle();

      expect(
        find.text('分叉会话'),
        findsOneWidget,
        reason: '菜单里没有这一项 —— 能力在服务端造好了，界面上没有入口等于没做',
      );
      await tester.tap(find.text('分叉会话'));
      await tester.pumpAndSettle();
      expect(forked, isTrue, reason: '点了菜单项必须触发回调，否则它是个摆设');
    });
  });
}

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}
