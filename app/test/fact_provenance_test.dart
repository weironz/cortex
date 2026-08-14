@TestOn('vm')
library;

import 'package:cortex_app/core/theme.dart';
import 'package:cortex_app/features/memory/widgets/fact_card.dart';
import 'package:cortex_app/models/memory_fact.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 「这条是我说的，还是模型猜的」—— 抽屉上此前完全看不出来。
///
/// `facts.source_channel` / `trust_tier` 从 migration 20260807000006 起就在
/// 写了，但 `FactDto` 不带这两个字段，于是一条**用户亲口说的**事实和一条
/// **模型从对话里推断的**事实在界面上长得一模一样。
///
/// 这对这个产品是最不该有的含糊：「为什么记得这个」正是它的卖点。
///
/// 顺带钉住 `confidence`：它曾经在 cortexd 里被写死成 `1.0`，于是每条记忆
/// 都显示「100%」—— 一个看起来像数据、实际是常量的数字。
void main() {
  MemoryFact fact({String? channel, int? tier, double? confidence}) =>
      MemoryFact(
        id: '01JF1',
        statement: '生产环境使用 Postgres 17 作为数据库',
        sourceChannel: channel,
        trustTier: tier,
        confidence: confidence,
      );

  Future<void> pump(WidgetTester tester, MemoryFact f) => tester.pumpWidget(
    MaterialApp(
      theme: CortexTheme.light(),
      home: Scaffold(body: FactCard(fact: f)),
    ),
  );

  group('来源角标', () {
    testWidgets('亲述与推断必须看得出区别', (tester) async {
      await pump(tester, fact(channel: 'user_stated', tier: 1));
      expect(
        find.text('你说的'),
        findsOneWidget,
        reason: '用户亲口说的事实必须标出来 —— 它与模型的推断可信度不同',
      );

      await pump(tester, fact(channel: 'conversation', tier: 2));
      expect(find.text('推断'), findsOneWidget);
      expect(find.text('你说的'), findsNothing, reason: '推断出来的事实被标成亲述，比不标更糟');
    });

    testWidgets('存量行不编一个「未知」出来', (tester) async {
      await pump(tester, fact(channel: 'unknown_legacy'));
      expect(find.text('unknown_legacy'), findsNothing, reason: '线上词汇不该出现在界面上');
      expect(
        find.text('未知'),
        findsNothing,
        reason:
            '加列之前的行确实无从得知来源。'
            '标「未知」是把噪声打扮成出处，不如不标',
      );

      // 字段整个缺失（老 daemon）走同一条路
      await pump(tester, fact());
      expect(find.text('未知'), findsNothing);
    });

    testWidgets('认不出的通道原样显示，而不是悄悄吞掉', (tester) async {
      await pump(tester, fact(channel: 'imported_from_chatgpt', tier: 4));
      expect(
        find.text('imported_from_chatgpt'),
        findsOneWidget,
        reason:
            '服务端新加一个来源通道时，它该在界面上出现。'
            '吞掉的话，一整类记忆会静默地失去出处标记',
      );
    });
  });

  group('可信度', () {
    testWidgets('拿不到时不显示，而不是显示成 100%', (tester) async {
      await pump(tester, fact(channel: 'user_stated', tier: 1));
      expect(
        find.textContaining('%'),
        findsNothing,
        reason:
            'confidence 缺失时画一个百分比，等于凭空造一个数字。'
            'cortexd 此前正是写死 1.0，于是每条都显示 100%',
      );
    });

    testWidgets('拿得到就照实显示', (tester) async {
      await pump(
        tester,
        fact(channel: 'conversation', tier: 2, confidence: 0.72),
      );
      expect(find.textContaining('72'), findsOneWidget, reason: '真实打分必须原样呈现');
    });
  });

  test('两个字段都从 JSON 读出来', () {
    final f = MemoryFact.fromJson(const {
      'id': '01JF1',
      'statement': 'x',
      'source_channel': 'tool_output',
      'trust_tier': 3,
    });
    expect(f.sourceChannel, 'tool_output');
    expect(f.trustTier, 3);

    // 老 daemon 不发这两个字段：必须是 null 而不是抛异常
    final old = MemoryFact.fromJson(const {'id': '01JF1', 'statement': 'x'});
    expect(old.sourceChannel, isNull);
    expect(old.trustTier, isNull);
  });

  /// 同一个形状的第三次：**服务端一直在发，客户端从来没读**。
  ///
  /// `FactDto.invalidated` 早就在协议里，而 `MemoryFact.fromJson` 直到现在
  /// 都没解析它。后果恰好落在最不该出问题的地方：**时间回放**。
  /// 回放到过去时，「当时成立、此后已被推翻」的事实与现行事实长得一模一样 ——
  /// 而那正是回放唯一有信息量的场合，也是这个产品对标 mem0 / zep 的差异点。
  /// 没有这一位，时间轴只是把同一份列表换了个好看的画法。
  ///
  /// 前两次是 `source_channel` / `trust_tier`（上面那条测试）与
  /// `confidence` 写死 1.0。三次都是同一句话：**加了字段不等于用上了字段**。
  group('invalidated —— 回放里唯一有信息量的那一位', () {
    test('从 JSON 读出来，且老 daemon 不发时按「没被推翻」算', () {
      final replayed = MemoryFact.fromJson(const {
        'id': '01JF1',
        'statement': 'x',
        'invalidated': true,
      });
      expect(replayed.invalidated, isTrue);

      final live = MemoryFact.fromJson(const {'id': '01JF1', 'statement': 'x'});
      expect(
        live.invalidated,
        isFalse,
        reason:
            '字段缺失要降级成「没有东西被推翻」。反过来（默认 true）会让整屏'
            '划掉的文字出现在一个只是 daemon 版本旧的部署上',
      );
    });

    testWidgets('这一位要真的到得了卡片', (tester) async {
      await pump(tester, fact(channel: 'user_stated', tier: 1));
      expect(
        tester.widget<FactCard>(find.byType(FactCard)).invalidated,
        isFalse,
      );

      await tester.pumpWidget(
        MaterialApp(
          theme: CortexTheme.light(),
          home: Scaffold(
            body: FactCard(
              fact: fact(channel: 'user_stated', tier: 1),
              invalidated: true,
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(
        tester.widget<FactCard>(find.byType(FactCard)).invalidated,
        isTrue,
        reason: '解析出来却不往下传，与不解析等价 —— 这个 bug 正是断在传递那一段',
      );
    });
  });
}
