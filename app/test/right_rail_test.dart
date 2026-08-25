/// 右栏：折叠语义、终端页签的判据、「在资源管理器中打开」的判据。
///
/// # 这三组各自盯着什么
///
/// 1. **栏头那个按钮是「收起」，不是「关闭」**，且图标与顶栏开关是同一个
///    常量。图标分叉不会有任何报错 —— 只是用户认不出「栏头收起去的，就是
///    顶栏那个能叫回来的」。
///
/// 2. **交互终端只在「本机 agent 活着」时出现**（约束 2）。判据只在
///    `_TerminalTab` 一处；这里钉住它的两个方向：没有 agent 必须退回只读
///    记录（摆一个连不上的终端就是在撒谎），有 agent 且有会话必须真的换成
///    交互终端（否则能力造好了没人接）。
///
/// 3. **「在资源管理器中打开」只在会话绑了本机目录时画**。云端会话的文件
///    在容器卷里，这台机器上没有对应目录 —— 画出来点了只会打开一个不存在
///    的路径。Web 侧由 `kCanRevealInFileManager`（编译期恒 false）整个裁掉，
///    VM 测试到不了那条路，这里测的是「绑定与否」这一半判据。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/workspace/interactive_terminal.dart';
import 'package:cortex_app/features/workspace/right_rail.dart';
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

/// 挂起整棵右栏。[agentOrigin] 模拟「本机 agent 活着没有」——
/// null 就是没起来（Web、flutter run、agent 崩了都长这样）。
Widget _app({String? agentOrigin, VoidCallback? onClose}) => ProviderScope(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
    localAgentOriginProvider.overrideWith((ref) => Future.value(agentOrigin)),
  ],
  child: MaterialApp(
    home: Scaffold(body: RightRail(onClose: onClose)),
  ),
);

/// 让 mock 的会话列表与 FutureProvider 落定。刻意用定长 pump 而不是
/// pumpAndSettle：文件树里有转圈动画，settle 会一直等到超时。
Future<void> _settle(WidgetTester tester) async {
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 50));
  }
}

ProviderContainer _containerOf(WidgetTester tester) =>
    ProviderScope.containerOf(tester.element(find.byType(RightRail)));

void main() {
  group('栏头是折叠，不是关闭', () {
    testWidgets('图标是右栏开关那一个，点了就收', (tester) async {
      var closed = false;
      await tester.pumpWidget(_app(onClose: () => closed = true));
      await _settle(tester);

      expect(
        find.byTooltip('收起右栏'),
        findsOneWidget,
        reason: '栏头那个按钮的语义是「收起」——它收起去的东西顶栏还能叫回来',
      );
      expect(
        find.descendant(
          of: find.byTooltip('收起右栏'),
          matching: find.byIcon(kRailToggleOutlined),
        ),
        findsOneWidget,
        reason:
            '图标必须是 kRailToggleOutlined —— 与顶栏开关同一个常量。'
            '各画各的图标，用户就认不出这俩是同一个开关',
      );
      expect(
        find.byIcon(Icons.close_rounded),
        findsNothing,
        reason: '「关闭 ✕」不该再出现：这一栏不是弹层，是与左栏对称的一列',
      );

      await tester.tap(find.byTooltip('收起右栏'));
      expect(closed, isTrue, reason: '按钮要真的接到收起回调上，不是只换了个图标');
    });
  });

  group('终端页签的判据（只在 _TerminalTab 一处）', () {
    testWidgets('本机 agent 没起来：不摆交互终端，退回只读命令记录', (tester) async {
      await tester.pumpWidget(_app());
      await _settle(tester);

      await tester.tap(find.text('终端'));
      await _settle(tester);

      expect(
        find.byType(InteractiveTerminal),
        findsNothing,
        reason:
            '没有活着的本机 agent 就没有 shell 可连。摆一个连不上的终端，'
            '就是 CLAUDE.md 约束 2 说的那种「界面替产品撒谎」',
      );
      expect(
        find.text('这个会话还没有跑过命令。'),
        findsOneWidget,
        reason: '退回的只读记录本身是真的（它显示模型跑过什么），不该一起消失',
      );
    });

    testWidgets('agent 活着且有会话：换成真的交互终端', (tester) async {
      await tester.pumpWidget(_app(agentOrigin: 'http://127.0.0.1:9'));
      await _settle(tester);

      // 有会话才有 cwd（跟着工作区绑定走）—— 建一个草稿会话即可
      _containerOf(
        tester,
      ).read(chatControllerProvider.notifier).createSession();
      await tester.pump();

      await tester.tap(find.text('终端'));
      await _settle(tester);

      expect(
        find.byType(InteractiveTerminal),
        findsOneWidget,
        reason:
            '判据满足时必须真的换成交互终端 —— 否则 Rust 侧的 PTY 端点'
            '就是「造好了没人调用」，这个仓库数过十来次的形状',
      );
      // 测试环境连不上 9 号端口是预期的（flutter_test 挡了真网络）；
      // 这里验的是判据接线，不是握手本身 —— 那在 Rust 侧的测试里
    });
  });

  group('「在资源管理器中打开」的判据', () {
    testWidgets('绑了本机目录的会话：按钮在；云端会话：按钮不在', (tester) async {
      await tester.pumpWidget(_app());
      await _settle(tester);
      final container = _containerOf(tester);
      final ctrl = container.read(chatControllerProvider.notifier);

      final sessions = container.read(chatControllerProvider).sessions;
      final bound = sessions.firstWhere(
        (s) => s.workspace != null,
        orElse: () => fail('mock 夹具里该有一条绑了工作区的会话（ses_01JQZ2N8D1）'),
      );
      ctrl.selectSession(bound.id);
      await _settle(tester);

      expect(
        find.byTooltip('在资源管理器中打开'),
        findsOneWidget,
        reason: '桌面端 + 有本机绑定：这正是这个按钮存在的唯一场景',
      );

      final cloud = sessions.firstWhere(
        (s) => s.workspace == null,
        orElse: () => fail('mock 夹具里该有一条没绑工作区的会话'),
      );
      ctrl.selectSession(cloud.id);
      await _settle(tester);

      expect(
        find.byTooltip('在资源管理器中打开'),
        findsNothing,
        reason:
            '云端会话的文件在容器卷里，这台机器上没有那个目录 —— '
            '按钮画出来点了只会打开一个不存在的路径',
      );
    });
  });
}
