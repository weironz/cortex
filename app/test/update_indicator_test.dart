/// 顶栏那个「关于 / 更新」小图标。
///
/// 它有一个安静的默认态和一个会被点的活跃态，而**默认态错了没人会发现**：
/// 一个永远不亮的图标与一个功能正常但恰好没有新版本的图标长得一模一样。
library;

import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/update_feed.dart';
import 'package:cortex_app/features/settings/pages/about_page.dart';
import 'package:cortex_app/features/shell/widgets/update_indicator.dart';
import 'package:cortex_app/state/update_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _release = UpdateRelease(
  version: '9.9.9',
  setupUrl: 'https://example.invalid/s.exe',
  setupName: 's.exe',
  sumsUrl: 'https://example.invalid/S',
);

/// 一个不碰网络也不碰磁盘的替身：只是把状态摆成我们要看的样子。
class _Fake extends UpdateController {
  _Fake(this._seed);
  final UpdateState _seed;
  int installs = 0;
  int retries = 0;

  @override
  UpdateState build() => _seed;

  @override
  bool get enabled => true;

  @override
  Future<void> install() async => installs++;

  @override
  Future<void> retry() async => retries++;

  @override
  Future<void> check() async {}
}

Future<void> _pump(WidgetTester tester, [_Fake? fake]) => tester.pumpWidget(
  ProviderScope(
    overrides: [
      if (fake != null) updateControllerProvider.overrideWith(() => fake),
    ],
    child: const MaterialApp(
      home: Scaffold(body: Center(child: UpdateIndicator())),
    ),
  ),
);

void main() {
  _aboutSubtitleTests();

  testWidgets('没有版本号的构建：图标在，但不带小红点，也不该提示更新', (tester) async {
    // 这个测试进程没有 `--dart-define=CORTEX_APP_VERSION`，也就是
    // 「开发构建」那条路 —— 与用户手上任何一份正式产物都不同
    expect(
      AppConfig.updatesSupported,
      isFalse,
      reason: '前置条件：测试构建不该自认为知道自己是哪一版',
    );

    await _pump(tester);
    expect(find.byIcon(Icons.help_outline_rounded), findsOneWidget);
    expect(
      find.byType(CircularProgressIndicator),
      findsNothing,
      reason: '闲着的时候不该有任何转圈的东西',
    );
  });

  testWidgets('有新版本时点一下就开始更新，不再多问一句', (tester) async {
    final fake = _Fake(
      const UpdateState(phase: UpdatePhase.available, release: _release),
    );
    await _pump(tester, fake);

    await tester.tap(find.byType(IconButton));
    await tester.pump();

    expect(
      fake.installs,
      1,
      reason:
          '用户要的就是「点击即自动下载并安装更新重启」。'
          '中间插一个确认框会让这个图标变成又一个要读的弹窗',
    );
  });

  testWidgets('下载中不可点 —— 免得点第二下再下一遍', (tester) async {
    final fake = _Fake(
      const UpdateState(
        phase: UpdatePhase.downloading,
        release: _release,
        progress: 0.4,
      ),
    );
    await _pump(tester, fake);

    expect(find.byType(CircularProgressIndicator), findsOneWidget);
    final button = tester.widget<IconButton>(find.byType(IconButton));
    expect(button.onPressed, isNull, reason: '正在下载时这个图标是状态，不是按钮');
  });

  testWidgets('失败之后点一下是重试，且 tooltip 说得出失败在哪', (tester) async {
    final fake = _Fake(
      const UpdateState(
        phase: UpdatePhase.failed,
        release: _release,
        error: '校验和对不上',
      ),
    );
    await _pump(tester, fake);

    expect(find.byIcon(Icons.error_outline_rounded), findsOneWidget);
    final tip = tester.widget<Tooltip>(find.byType(Tooltip));
    expect(
      tip.message,
      contains('校验和对不上'),
      reason: '「更新失败」四个字不可行动。失败在哪一步才是用户能拿去做点什么的',
    );

    await tester.tap(find.byType(IconButton));
    await tester.pump();
    expect(fake.retries, 1);
  });

  testWidgets('下好了但正在回答：说清在等什么，而不是默默卡住', (tester) async {
    final fake = _Fake(
      const UpdateState(
        phase: UpdatePhase.ready,
        release: _release,
        waitingForTurn: true,
      ),
    );
    await _pump(tester, fake);

    final tip = tester.widget<Tooltip>(find.byType(Tooltip));
    expect(
      tip.message,
      contains('这轮回答结束后'),
      reason:
          '安装会把应用关掉。正在流式回答时关掉它，那一轮就丢在半路上了 —— '
          '而用户只知道自己点了「更新」，不知道为什么要等',
    );
  });
}

/// **「关于」那一页不许说一句它没验证过的话。**
///
/// `UpdatePhase.idle` 同时是「查过了，已是最新」与「还没查」。把后一种也
/// 写成「已是最新」，一个 0.1.24 的用户打开这一页会被告知自己是最新的 ——
/// 然后他就关掉了。**而线上摆着 0.1.25。**
///
/// 2026-08-28 真机实测撞到的：本机自称 0.1.0、release 页上是 0.1.25，
/// 这一页写着「已是最新」，因为 24 小时节流让那次启动跳过了检查。
void _aboutSubtitleTests() {
  Future<void> pump(WidgetTester tester, UpdateState seed) async {
    await tester.pumpWidget(
      ProviderScope(
        overrides: [updateControllerProvider.overrideWith(() => _Fake(seed))],
        child: const MaterialApp(home: Scaffold(body: AboutTile())),
      ),
    );
    await tester.pump();
  }

  testWidgets('没查过的时候不许说「已是最新」', (tester) async {
    await pump(tester, const UpdateState());
    expect(
      find.text('已是最新'),
      findsNothing,
      reason:
          '还没查过就断言「已是最新」—— 那句话没有任何依据，'
          '而它恰好长得像一句好消息',
    );
    expect(find.text('还没检查过'), findsOneWidget);
  });

  testWidgets('查过且确实是最新，才说「已是最新」', (tester) async {
    await pump(tester, UpdateState(checkedAt: DateTime(2026, 8, 28)));
    expect(
      find.text('已是最新'),
      findsOneWidget,
      reason: '查过了就该说结论 —— 否则用户永远看不到「好了，你是最新的」',
    );
  });
}
