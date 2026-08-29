/// 账号页：昵称、头像、口令、删号。
///
/// # 这一组守着什么
///
/// 三条都是**错了不报错**的：
///
/// 1. `Patch` 那层「不动 vs 清空」的区别 —— 少了它，清空昵称发不出去，
///    而界面上看起来像保存成功了（输入框里就是空的）。
/// 2. 没有账号体系的部署（老服务端 / 预共享 token）要**说清楚**，
///    而不是画一堆点了没反应的输入框。
/// 3. 删号那条路上「撤销只有 15 分钟」这句话必须在界面上 —— 用户以为
///    「7 天内随时能反悔」的代价是全部历史。
library;

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/features/settings/pages/account_page.dart';
import 'package:cortex_app/models/account.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 记下 `updateProfile` 收到了什么 —— 这一层的重点是**发出去的形状**。
class _SpyApi extends MockCortexApi {
  _SpyApi() : super(instant: true);

  /// 每次调用记一条：`null` = 这次没提昵称，`Patch(x)` = 提了。
  final calls = <Patch<String?>?>[];

  @override
  Future<Profile> updateProfile({Patch<String?>? nickname}) {
    calls.add(nickname);
    return super.updateProfile(nickname: nickname);
  }
}

/// 一个「答不出账号资料」的后端 —— 老服务端与预共享 token 部署的样子。
class _NoAccountsApi extends MockCortexApi {
  _NoAccountsApi() : super(instant: true);

  @override
  Future<Profile> profile() async =>
      throw const CortexApiException('没有这条路由', statusCode: 404);
}

Future<void> _pump(WidgetTester tester, ProviderContainer c) async {
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: const MaterialApp(home: Scaffold(body: AccountPage())),
    ),
  );
  // profile 是异步读的，等它落定
  for (var i = 0; i < 8; i++) {
    await tester.pump(const Duration(milliseconds: 20));
  }
}

/// 收尾：把 mock 的仿真延迟与 riverpod 的 dispose 任务都走完。
///
/// 不做的话测试结束时挂着 pending timer，红在一句与本测试无关的断言上
/// （`A Timer is still pending`）—— 与 `stream_idle_test` 同一个坑。
Future<void> _settleAndDispose(WidgetTester tester, ProviderContainer c) async {
  for (var i = 0; i < 8; i++) {
    await tester.pump(const Duration(milliseconds: 400));
  }
  await tester.pumpWidget(const SizedBox.shrink());
  await tester.pump();
  await tester.pump();
}

void main() {
  testWidgets('清空昵称发的是 Patch(null)，不是「这次没提」', (tester) async {
    final api = _SpyApi();
    final c = ProviderContainer(
      overrides: [cortexApiProvider.overrideWithValue(api)],
    );
    addTearDown(c.dispose);
    await _pump(tester, c);

    // mock 的初始昵称是「演示账号」，把它删光再保存
    await tester.enterText(find.byType(TextField).first, '');
    await tester.tap(find.widgetWithText(FilledButton, '保存'));
    await tester.pump();
    for (var i = 0; i < 8; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }

    expect(api.calls, isNotEmpty, reason: '按了保存却什么都没发出去');
    expect(
      api.calls.last,
      isNotNull,
      reason:
          '清空昵称必须**提到**这个字段（Patch(null)）—— '
          '不提的话服务端理解成「这次不动它」，昵称原封不动，'
          '而界面上输入框是空的，看起来像保存成功了',
    );
    expect(api.calls.last!.value, isNull, reason: 'Patch 里要是 null 才是「清空」');
  });

  testWidgets('没有账号体系的部署：说清楚，而不是画一堆没用的输入框', (tester) async {
    final c = ProviderContainer(
      overrides: [cortexApiProvider.overrideWithValue(_NoAccountsApi())],
    );
    addTearDown(c.dispose);
    await _pump(tester, c);

    expect(
      find.textContaining('这个部署没有账号体系'),
      findsOneWidget,
      reason: '答不出资料时要解释原因 —— 画一堆点了没反应的输入框最糟',
    );
    expect(find.byType(TextField), findsNothing, reason: '既然改不了，就不该摆输入框');
    await _settleAndDispose(tester, c);
  });

  testWidgets('排期删除时，「撤销只有 15 分钟」必须写在脸上', (tester) async {
    final api = _SpyApi();
    final c = ProviderContainer(
      overrides: [cortexApiProvider.overrideWithValue(api)],
    );
    addTearDown(c.dispose);
    // 直接把 mock 的资料造成「已排期」
    api.setProfileForTest(
      Profile(
        userId: 'mock-user',
        username: 'demo',
        purgeAfter: DateTime.now().add(const Duration(days: 7)),
      ),
    );
    await _pump(tester, c);

    expect(find.textContaining('将在'), findsOneWidget, reason: '要显示倒计时');
    expect(
      find.textContaining('15 分钟'),
      findsOneWidget,
      reason:
          '排期之后登录被拒，撤销只能靠手上这把还没过期的 token。'
          '不写出来，用户会以为「7 天内随时能反悔」—— 而那个误会的代价是全部历史',
    );
    expect(find.widgetWithText(FilledButton, '撤销删除'), findsOneWidget);
    await _settleAndDispose(tester, c);
  });
}
