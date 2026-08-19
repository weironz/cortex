/// 用量与花费。
///
/// 这一页最容易出的错不是崩，而是**给出一个编出来的数字**：
///
/// 1. 没有价目的模型按 0 显示成「¥0.00」—— 读起来是免费，事实是不知道
/// 2. 不限量的部署上把 `limit - used` 算成负数
/// 3. 一次对话只花几厘，四舍五入到分之后永远是 ¥0.00
/// 4. 没接账号体系的部署（404）被画成一条用户消不掉的红字
library;

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/features/settings/pages/usage_page.dart';
import 'package:cortex_app/models/usage_report.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _Api extends MockCortexApi {
  // ⚠️ 这个字段不能叫 override：它会遮住 `@override` 注解，
  // 而报错是「Not a constant expression」，指向下一行的注解，
  // 与真正的原因隔着一层
  _Api({this.report, this.failure});

  final UsageReport? report;
  final CortexApiException? failure;

  @override
  Future<UsageReport> usage() async {
    if (failure != null) throw failure!;
    return report ?? await super.usage();
  }
}

Future<void> _pump(WidgetTester tester, _Api api) async {
  tester.view.physicalSize = const Size(900, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);

  await tester.pumpWidget(
    ProviderScope(
      overrides: [cortexApiProvider.overrideWithValue(api)],
      child: const MaterialApp(home: Scaffold(body: UsagePage())),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  group('金额格式', () {
    test('几厘也要看得见', () {
      expect(
        formatMoney(3200, 'CNY'),
        '¥0.0032',
        reason:
            '一次对话常常只花几厘。四舍五入到分之后它永远是 ¥0.00，'
            '而这正是用户拿来判断「这个功能贵不贵」的那个数',
      );
    });

    test('零就是零，不多给位数', () {
      expect(formatMoney(0, 'CNY'), '¥0.00');
    });

    test('大额按两位', () {
      expect(formatMoney(2729748, 'CNY'), '¥2.73');
    });

    test('认不出的货币也要显示得出来', () {
      expect(
        formatMoney(1000000, 'JPY'),
        'JPY 1.00',
        reason: '换个供应商配成别的币种时，界面不该变成一个空符号',
      );
    });
  });

  group('token 格式', () {
    test('加千位分隔', () {
      expect(formatTokens(1234567), '1,234,567');
      expect(formatTokens(999), '999');
      expect(formatTokens(1000), '1,000');
      expect(formatTokens(0), '0');
    });
  });

  group('比例', () {
    test('不限量时没有比例，而不是除以零', () {
      const r = UsageReport(usedTokens: 500);
      expect(
        r.ratio,
        isNull,
        reason: '不限量时不该出现进度条 —— 一条画不出终点的进度条只会让人猜',
      );
      expect(r.limited, isFalse);
    });

    test('超额时封在 1，不会画出一条溢出的条', () {
      const r = UsageReport(usedTokens: 3000000, limitTokens: 2000000);
      expect(r.ratio, 1.0);
    });
  });

  group('页面', () {
    testWidgets('没有价目的模型显示「没有价目」，不是 ¥0.00', (tester) async {
      await _pump(tester, _Api());

      expect(
        find.text('没有价目'),
        findsOneWidget,
        reason:
            '夹具里那个 local-qwen3-32b 没有价目。显示成 ¥0.00 的话，'
            '用户会以为它免费 —— 而事实是这个部署不知道它多少钱',
      );
      expect(
        find.textContaining('不包含'),
        findsOneWidget,
        reason: '总额里少算了那部分，这件事必须当场说，而不是让用户自己发现加不起来',
      );
    });

    testWidgets('配额进度与剩余量都画出来', (tester) async {
      await _pump(tester, _Api());

      expect(find.byType(LinearProgressIndicator), findsOneWidget);
      expect(find.textContaining('还剩 832,921'), findsOneWidget);
      expect(
        find.textContaining('不占配额'),
        findsOneWidget,
        reason: '自带 key 那部分为什么不算进配额，要在它旁边说',
      );
    });

    testWidgets('不限量的部署不画进度条，而是说清楚', (tester) async {
      await _pump(
        tester,
        _Api(
          report: const UsageReport(
            usedTokens: 500,
            costMicros: 1200,
            currency: 'CNY',
          ),
        ),
      );

      expect(find.byType(LinearProgressIndicator), findsNothing);
      expect(find.text('这个部署不限额度。'), findsOneWidget);
    });

    testWidgets('没接账号体系的部署说「不记账」，不是一条红字', (tester) async {
      await _pump(
        tester,
        _Api(
          failure: const CortexApiException('Not Found', statusCode: 404),
        ),
      );

      expect(find.text('这个部署不记用量'), findsOneWidget);
      expect(
        find.text('重试'),
        findsNothing,
        reason: '没有这个功能不是重试能解决的事 —— 给个重试按钮只会让人一直点',
      );
    });

    testWidgets('真出错时给得出重试', (tester) async {
      await _pump(
        tester,
        _Api(failure: const CortexApiException('库连不上', statusCode: 500)),
      );

      expect(find.text('拉不到用量'), findsOneWidget);
      expect(find.text('重试'), findsOneWidget);
    });

    testWidgets('一条记录都没有时不显示空白', (tester) async {
      await _pump(tester, _Api(report: const UsageReport(windowDays: 30)));

      expect(
        find.textContaining('还没有调用记录'),
        findsOneWidget,
        reason: '新账号打开这一页看到一片空白，会以为页面坏了',
      );
    });
  });
}
