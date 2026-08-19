/// 会话搜索。
///
/// 这一组盯住的都是「不报错的错」——搜索坏掉时几乎从不抛异常，
/// 它只是给出一份看起来很正常、实际不对的结果：
///
/// 1. 慢请求把旧词的结果盖回界面（在飞请求不作废）
/// 2. 清空搜索框变成「列出全部」（空串顶掉默认值，本仓库第七次）
/// 3. 老服务端 404 被画成一条用户消不掉的红字，而不是安静地不给搜索框
/// 4. 打字过程中每敲一下都发一次全表 ILIKE（中文输入法一个词发七八次）
library;

import 'dart:async';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/app.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/models/session_search_hit.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/session_search_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

// ---------------------------------------------------------------- 测试替身

/// 记下每一次搜索，并让每次调用的完成时刻由测试说了算。
class _SearchApi extends MockCortexApi {
  final List<String> queries = [];
  final List<Completer<List<SessionSearchHit>>> pending = [];

  /// 非 null 时所有搜索都以它失败。
  CortexApiException? failure;

  /// 挂起：不自动完成，交给测试在合适的时刻放行。
  bool hang = false;

  @override
  Future<List<SessionSearchHit>> searchSessions(
    String query, {
    bool includeArchived = false,
  }) {
    queries.add(query);
    if (failure != null) return Future.error(failure!);
    if (!hang) {
      return Future.value([
        SessionSearchHit(
          sessionId: 's-$query',
          title: '关于 $query 的会话',
          titleMatch: true,
        ),
      ]);
    }
    final c = Completer<List<SessionSearchHit>>();
    pending.add(c);
    return c.future;
  }
}

ProviderContainer _boot(_SearchApi api) =>
    ProviderContainer(overrides: [cortexApiProvider.overrideWithValue(api)]);

/// 跑完排在微任务里的东西。
Future<void> _drain([int turns = 4]) async {
  for (var i = 0; i < turns; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

/// 等过防抖窗口。
Future<void> _pastDebounce() =>
    Future<void>.delayed(const Duration(milliseconds: 400));

void main() {
  group('搜索控制器', () {
    test('打字要防抖：连敲不会一个字发一次', () async {
      final api = _SearchApi();
      final container = _boot(api);
      addTearDown(container.dispose);

      final ctrl = container.read(sessionSearchProvider.notifier);
      // 模拟中文输入法：一个词打完之前 onChanged 触发好几次
      ctrl.setQuery('g');
      ctrl.setQuery('go');
      ctrl.setQuery('gon');
      ctrl.setQuery('工作');
      ctrl.setQuery('工作区');
      await _pastDebounce();

      expect(
        api.queries,
        ['工作区'],
        reason:
            '五次击键只该发一次请求；每敲一下发一次的话，'
            '打一个中文词就是七八次全表 ILIKE',
      );
    });

    test('输入框里的词立刻生效，不等防抖', () {
      final api = _SearchApi();
      final container = _boot(api);
      addTearDown(container.dispose);

      container.read(sessionSearchProvider.notifier).setQuery('合同');

      expect(
        container.read(sessionSearchProvider).active,
        isTrue,
        reason:
            'active 跟着输入框走而不是跟着请求走 —— '
            '否则敲完到防抖到期那 250 毫秒里界面还画着会话列表，'
            '看起来像输入被吞了',
      );
    });

    test('慢请求不许把旧词的结果盖回来', () async {
      final api = _SearchApi()..hang = true;
      final container = _boot(api);
      addTearDown(container.dispose);

      final ctrl = container.read(sessionSearchProvider.notifier);
      ctrl.setQuery('旧词');
      await _pastDebounce();
      expect(api.pending, hasLength(1), reason: '第一次请求已经发出去了');

      ctrl.setQuery('新词');
      await _pastDebounce();
      expect(api.pending, hasLength(2));

      // 新的先回，旧的后回 —— 真机上完全可能
      api.pending[1].complete([
        const SessionSearchHit(sessionId: 's-new', title: '新词命中'),
      ]);
      await _drain();
      api.pending[0].complete([
        const SessionSearchHit(sessionId: 's-old', title: '旧词命中'),
      ]);
      await _drain();

      final hits = container.read(sessionSearchProvider).hits;
      expect(
        hits.map((h) => h.sessionId),
        ['s-new'],
        reason:
            '迟到的旧请求必须被丢掉。不丢的话，用户看到的是自己'
            '**上一个**搜索词的结果，而搜索框里写着新词',
      );
    });

    test('清空搜索框 = 回到会话列表，而不是「列出全部」', () async {
      final api = _SearchApi();
      final container = _boot(api);
      addTearDown(container.dispose);

      final ctrl = container.read(sessionSearchProvider.notifier);
      ctrl.setQuery('预算');
      await _pastDebounce();
      expect(container.read(sessionSearchProvider).hits, isNotEmpty);

      ctrl.setQuery('');
      await _pastDebounce();

      final state = container.read(sessionSearchProvider);
      expect(state.active, isFalse, reason: '空词不激活搜索');
      expect(state.hits, isEmpty, reason: '上一次的结果要清掉，否则它会挂在那儿');
      expect(api.queries, [
        '预算',
      ], reason: '空词一次请求都不该发 —— ILIKE %% 匹配一切，那是一次全表扫描');
    });

    test('只有空格也当空处理', () async {
      final api = _SearchApi();
      final container = _boot(api);
      addTearDown(container.dispose);

      container.read(sessionSearchProvider.notifier).setQuery('   ');
      await _pastDebounce();

      expect(container.read(sessionSearchProvider).active, isFalse);
      expect(api.queries, isEmpty, reason: '全是空格的模式能匹配上任何带空格的句子');
    });

    test('404 归为「这个后端没这功能」，不是错误', () async {
      final api = _SearchApi()
        ..failure = const CortexApiException('Not Found', statusCode: 404);
      final container = _boot(api);
      addTearDown(container.dispose);

      container.read(sessionSearchProvider.notifier).setQuery('随便');
      await _pastDebounce();
      await _drain();

      final state = container.read(sessionSearchProvider);
      expect(state.unsupported, isTrue);
      expect(
        state.error,
        isNull,
        reason:
            '老服务端没这条路由不是用户的错，也不是他能处理的事。'
            '画成红字的话那条红字他永远消不掉',
      );
    });

    test('真出错时要给得出错误，而不是静静地没结果', () async {
      final api = _SearchApi()
        ..failure = const CortexApiException('数据库连不上', statusCode: 500);
      final container = _boot(api);
      addTearDown(container.dispose);

      container.read(sessionSearchProvider.notifier).setQuery('随便');
      await _pastDebounce();
      await _drain();

      final state = container.read(sessionSearchProvider);
      expect(state.error, '数据库连不上');
      expect(
        state.unsupported,
        isFalse,
        reason: '500 不是「没这功能」——把它也归过去的话，搜索会永远静默地什么都不返回',
      );
    });

    test('换后端时清空：旧结果里的会话 id 在新后端上不存在', () async {
      final first = _SearchApi();
      final second = _SearchApi();
      var swapped = false;
      final container = ProviderContainer(
        overrides: [
          cortexApiProvider.overrideWith((ref) => swapped ? second : first),
        ],
      );
      addTearDown(container.dispose);

      final ctrl = container.read(sessionSearchProvider.notifier);
      ctrl.setQuery('预算');
      await _pastDebounce();
      expect(container.read(sessionSearchProvider).hits, isNotEmpty);

      swapped = true;
      container.invalidate(cortexApiProvider);
      await _drain();

      expect(container.read(sessionSearchProvider).hits, isEmpty);
      expect(container.read(sessionSearchProvider).active, isFalse);
    });
  });

  group('侧边栏', () {
    Future<void> boot(WidgetTester tester) async {
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
    }

    testWidgets('搜出来的结果点得进去', (tester) async {
      await boot(tester);

      final box = find.widgetWithText(TextField, '搜索会话与消息');
      expect(box, findsOneWidget, reason: '侧栏顶上该有一个搜索框');

      // mock 的夹具里有一条讲 OKR 的会话
      await tester.enterText(box, 'OKR');
      await tester.pump(const Duration(milliseconds: 400));
      await tester.pumpAndSettle();

      final container = ProviderScope.containerOf(
        tester.element(find.byType(CortexApp)),
      );
      final hits = container.read(sessionSearchProvider).hits;
      expect(hits, isNotEmpty, reason: 'mock 夹具里有讲 OKR 的会话');

      // 结果列表画出来了，而不是那份完整的会话列表
      expect(
        find.textContaining('没有匹配的会话'),
        findsNothing,
        reason: '有结果时不该显示空态',
      );
    });

    testWidgets('搜不到时说清楚搜的是什么范围', (tester) async {
      await boot(tester);

      await tester.enterText(
        find.widgetWithText(TextField, '搜索会话与消息'),
        '这几个字全库都不会有zzz',
      );
      await tester.pump(const Duration(milliseconds: 400));
      await tester.pumpAndSettle();

      expect(find.text('没有匹配的会话'), findsOneWidget);
      expect(
        find.textContaining('已归档'),
        findsOneWidget,
        reason:
            '空结果最常见的原因就是「那条会话被归档了」。'
            '不说的话用户会以为消息丢了',
      );
    });
  });
}

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}
