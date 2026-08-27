/// 设置 → 我的机器。
///
/// # 这一组盯着三件「不会报错地错」的事
///
/// 1. **空态不能读成故障。** 一台在线机器都没有，最常见的原因是那几台机器
///    此刻关着 —— 说成「读不出」会让人去查网络、去重装。
/// 2. **「没开放接入」不是错误状态。** 它是默认值。给它一个红章会让一整列
///    默认状态的机器看起来像一列故障，而它们什么问题都没有；而且必须说清
///    **怎么开**，否则用户唯一能做的是挨个试。
/// 3. **「在线」不写出来。** 列表里每一台都在线（离线的服务端根本不回），
///    写了等于每行挂一枚恒真的章 —— 而恒真的章会训练人忽略那个位置。
library;

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/settings/pages/machines_page.dart';
import 'package:cortex_app/models/agent_presence.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

Future<void> _pump(
  WidgetTester tester,
  Stream<List<AgentPresence>> stream,
) async {
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        machinesProvider.overrideWith((ref) => stream),
      ],
      child: const MaterialApp(
        home: Scaffold(
          body: SizedBox(width: 900, height: 900, child: MachinesPage()),
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 100));
}

const _online = AgentPresence(
  agentId: 'a1',
  machineHint: 'WILLOPTPC',
  lastSeenSecs: 3,
  sessionCount: 12,
  attachable: true,
);
const _localOnly = AgentPresence(
  agentId: 'a2',
  machineHint: 'macbook-air',
  lastSeenSecs: 120,
  sessionCount: 0,
);

void main() {
  testWidgets('一台都没有时说的是「都关着」，不是「出错了」', (tester) async {
    await _pump(tester, Stream.value(const []));

    expect(find.textContaining('现在没有在线的机器'), findsOneWidget);
    expect(
      find.textContaining('半分钟内出现在这里'),
      findsOneWidget,
      reason: '要说清怎么让它出现 —— 只说「没有」等于把问题丢回给用户',
    );
    for (final scary in ['读不出', '失败', '错误']) {
      expect(
        find.textContaining(scary),
        findsNothing,
        reason: '空态被读成故障的话，用户会去查网络、去重装 —— 而机器只是关着',
      );
    }
  });

  testWidgets('每台机器：名字、多久前报到、几个会话', (tester) async {
    await _pump(tester, Stream.value(const [_online, _localOnly]));

    expect(find.text('WILLOPTPC'), findsOneWidget);
    expect(find.text('macbook-air'), findsOneWidget);
    expect(find.textContaining('刚刚报到'), findsOneWidget);
    expect(find.textContaining('2 分钟前报到'), findsOneWidget);
    expect(find.textContaining('12 个会话绑在它上面'), findsOneWidget);
    expect(
      find.textContaining('还没有绑定本机目录的会话'),
      findsOneWidget,
      reason: '0 个要说成一句话，而不是「0 个会话」—— 后者读起来像出了问题',
    );
  });

  /// ⚠️ 列表里**每一台都在线**（离线的服务端根本不回）。
  /// 写一枚「在线」的章等于每行挂一个恒真的东西 —— 而恒真的章会训练人
  /// 忽略那个位置，等它哪天真有第二种值时那里早已是盲区。
  testWidgets('不给每一行挂一枚恒真的「在线」章', (tester) async {
    await _pump(tester, Stream.value(const [_online, _localOnly]));

    // 判据是**每行的副标题里**没有「在线」——不是整页没有：页头副标题本来
    // 就写着「在线的 agent」，那是这一页的名字。
    //
    // ⚠️ 第一版写的是「渲染 1 台与 3 台时『在线』的出现次数相同」。
    // 它**永远绿**：同一个测试里第二次 pumpWidget 时 ProviderScope 被复用，
    // 覆盖不生效，两次数出来都是 2。故障注入（给每行加「在线 ·」）查出来的。
    final subtitles = tester
        .widgetList<Text>(find.textContaining('报到'))
        .map((t) => t.data ?? '')
        .toList();
    expect(subtitles, hasLength(2), reason: '两台机器该有两行副标题');
    for (final line in subtitles) {
      expect(
        line.contains('在线'),
        isFalse,
        reason:
            '列表里每台都在线，写出来是零信息量 —— '
            '而恒真的章会训练人忽略那个位置：$line',
      );
    }
  });

  group('够不够得着这一格', () {
    testWidgets('可接入的那台有色，说得出能干什么', (tester) async {
      await _pump(tester, Stream.value(const [_online]));

      expect(find.text('可接入'), findsOneWidget);
      final tip = tester.widget<Tooltip>(
        find.ancestor(of: find.text('可接入'), matching: find.byType(Tooltip)),
      );
      expect(tip.message, contains('接过去'), reason: '要说清这个标记意味着用户能做什么');
    });

    /// 「没开」是**默认状态**，不是故障 —— 给它错误色会让一整列默认状态的
    /// 机器看起来像一列故障。而 tooltip 必须说清**怎么开**。
    testWidgets('没开放的那台不用错误色，但要说清怎么开', (tester) async {
      await _pump(tester, Stream.value(const [_localOnly]));

      expect(find.text('仅本机'), findsOneWidget);
      final theme = Theme.of(tester.element(find.byType(MachinesPage)));
      final label = tester.widget<Text>(find.text('仅本机'));
      expect(
        label.style?.color,
        isNot(theme.colorScheme.error),
        reason: '默认状态用错误色 = 一整列没问题的机器看起来像一列故障',
      );

      final tip = tester.widget<Tooltip>(
        find.ancestor(of: find.text('仅本机'), matching: find.byType(Tooltip)),
      );
      expect(
        tip.message,
        contains('--allow-remote-attach'),
        reason: '只说「没开」不说怎么开，用户唯一能做的是挨个试',
      );
    });
  });

  testWidgets('老服务端没有这条路时，说的是「这个部署没有」', (tester) async {
    await _pump(
      tester,
      Stream.error(const CortexApiException('这个后端答不出在线名册。', statusCode: 404)),
    );

    expect(
      find.textContaining('升级服务端之后就有了'),
      findsOneWidget,
      reason: '说成「出错了」会让人去重试、去重启，而这条路根本不存在',
    );
  });
}
