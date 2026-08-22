/// 离线这件事在**会话列表**上说得清楚吗。
///
/// # 为什么单开一个文件
///
/// 这两条断言防的是同一类错误：**答案离问题太远**。
///
/// - 待补写的条数此前只在 设置 → 连接 那一页；而用户是在会话列表上
///   发现「我昨天那段对话不见了」的。
/// - 标题 / 项目 / 归档改不了这件事，此前要等他打完字点完确定，
///   才由一个 SnackBar 事后告知。
///
/// 两条都不是「功能坏了」，所以任何功能测试都抓不到它们。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/sessions/session_list.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

HealthStatus _health({int backlog = 0}) => HealthStatus(
  status: 'ok',
  version: '0.1.14',
  database: 'unknown',
  role: HealthStatus.roleLocalAgent,
  server: ServerLink(
    remote: 'https://example.com/api',
    reachable: true,
    backlog: backlog,
  ),
);

Future<void> _pump(
  WidgetTester tester, {
  required int backlog,
  bool offline = false,
}) async {
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        // 假数据源 —— 这两条断言看的是列表**顶上**那一条，与列表里装什么
        // 无关，省掉一整套 HTTP 桩。`instant` 不能少：默认那份带人工延时，
        // 测试结束时定时器还挂着，报的是「A Timer is still pending」
        // 而不是任何与断言有关的话
        appConfigProvider.overrideWith(() => _Config(offline: offline)),
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        healthProvider.overrideWith((ref) async => _health(backlog: backlog)),
      ],
      child: const MaterialApp(
        home: Scaffold(body: SizedBox(width: 320, child: SessionList())),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('有积压时，会话列表顶上就说得出还剩几条', (tester) async {
    await _pump(tester, backlog: 3);

    expect(
      find.textContaining('还有 3 条'),
      findsOneWidget,
      reason:
          '这个数字此前只在 设置 → 连接 那一页 —— 离「我的对话不见了」'
          '这个疑问四次点击远，等于没有答案',
    );
    expect(
      find.textContaining('完成后会出现在这份列表里'),
      findsOneWidget,
      reason:
          '要说清它会自己好。不说的话，用户会去找一个并不存在的'
          '「立即同步」按钮',
    );
  });

  testWidgets('没有积压时一个字都不画', (tester) async {
    await _pump(tester, backlog: 0);

    expect(
      find.textContaining('在补写'),
      findsNothing,
      reason:
          '常驻一句「0 条待同步」是噪音，而噪音会让真的有积压的那一次'
          '也被一起忽略掉',
    );
  });

  // ── 离线时那三项是灰的 ────────────────────────────────────

  /// 单独挂一个 [SessionTileMenu] 并把它点开。
  ///
  /// **不走真实列表**：那里这个按钮被一道悬停门挡着
  /// （`_SessionTile` 只在 `_hovered || selected` 时才让它 `enabled`），
  /// widget 测试里模拟悬停与模拟选中都试过，都不稳定 —— 而菜单的全部内容
  /// 只在弹出之后才存在，点不开就一条也断言不了。
  Future<void> openMenu(WidgetTester tester, {required bool offline}) async {
    await tester.pumpWidget(
      ProviderScope(
        overrides: [
          appConfigProvider.overrideWith(() => _Config(offline: offline)),
        ],
        child: MaterialApp(
          home: Scaffold(
            body: SessionTileMenu(
              archived: false,
              enabled: true,
              canMove: true,
              onRename: () {},
              onTogglePin: () {},
              pinned: false,
              onToggleArchive: () {},
              onMove: () {},
              onExport: (_) {},
            ),
          ),
        ),
      ),
    );
    await tester.tap(find.byIcon(Icons.more_horiz_rounded));
    await tester.pumpAndSettle();
    expect(find.text('重命名'), findsOneWidget, reason: '菜单没打开，下面的断言都无意义');
  }

  /// 菜单里这一项现在能不能点。
  ///
  /// 按**文字**回溯到它所在的 `PopupMenuEntry`，而不是按类型直接找：
  /// 那几项的类型参数并不一致（带 `value` 的是 `PopupMenuItem<String>`，
  /// 那条纯解释的没有 value），按具体类型找会漏掉一半。
  bool enabledOf(WidgetTester tester, String label) {
    final entry = find
        .ancestor(
          of: find.text(label),
          matching: find.byWidgetPredicate((w) => w is PopupMenuItem),
        )
        .evaluate()
        .first
        .widget;
    return (entry as PopupMenuItem).enabled;
  }

  testWidgets('离线时改标题/归档是灰的，且说清为什么', (tester) async {
    await openMenu(tester, offline: true);

    for (final label in ['重命名', '归档']) {
      expect(
        enabledOf(tester, label),
        isFalse,
        reason:
            '「$label」的权威在服务器上，离线时那条转发必然 502。'
            '让它可点的话，用户要打完字点完确定才被一个 SnackBar 告知'
            '这件事做不了 —— 事前说比事后说好',
      );
    }
    expect(
      find.textContaining('离线时改不了'),
      findsOneWidget,
      reason:
          '灰掉而不说原因等于让人以为界面坏了。而解释只给一次 —— '
          '给每一项各加一个后缀是同一句话说三遍',
    );
    expect(
      enabledOf(tester, '导出 Markdown'),
      isTrue,
      reason: '导出只发生在这台机器上，离线照样能做。一起灰掉是过度收缩',
    );
  });

  testWidgets('连着的时候它们照常可点，也不出现那句解释', (tester) async {
    await openMenu(tester, offline: false);

    expect(enabledOf(tester, '重命名'), isTrue);
    expect(enabledOf(tester, '归档'), isTrue);
    expect(
      find.textContaining('离线时改不了'),
      findsNothing,
      reason: '在线时多一行灰字，是给所有人看一条只对少数处境成立的说明',
    );
  });
}

class _Config extends AppConfigNotifier {
  _Config({required this.offline});
  final bool offline;

  @override
  AppConfig build() => AppConfig(
    useMock: true,
    baseUrl: 'http://127.0.0.1:8080',
    offline: offline,
  );
}
