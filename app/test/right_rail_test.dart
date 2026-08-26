/// 右栏与终端面板：折叠语义、终端的判据、「在资源管理器中打开」的判据。
///
/// # 这三组各自盯着什么
///
/// 1. **栏头那个按钮是「收起」，不是「关闭」**，且图标与顶栏开关是同一个
///    常量。图标分叉不会有任何报错 —— 只是用户认不出「栏头收起去的，就是
///    顶栏那个能叫回来的」。
///
/// 2. **交互终端只在「本机 agent 活着」时出现**（约束 2）。判据只在
///    `TerminalPanel` 一处；这里钉住它的两个方向：没有 agent 必须退回只读
///    记录（摆一个连不上的终端就是在撒谎），有 agent 且有会话必须真的换成
///    交互终端（否则能力造好了没人接）。终端 2026-08-26 从右栏的页签里
///    搬了出来，这一组跟着搬。
///
/// 3. **「在资源管理器中打开」只在会话绑了本机目录时画**。云端会话的文件
///    在容器卷里，这台机器上没有对应目录 —— 画出来点了只会打开一个不存在
///    的路径。Web 侧由 `kCanRevealInFileManager`（编译期恒 false）整个裁掉，
///    VM 测试到不了那条路，这里测的是「绑定与否」这一半判据。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/theme.dart';
import 'package:cortex_app/features/workspace/interactive_terminal.dart';
import 'package:cortex_app/features/workspace/right_rail.dart';
import 'package:cortex_app/features/workspace/terminal_panel.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/terminal_panel.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:xterm/xterm.dart';

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

  group('终端面板的判据（只在 TerminalPanel 一处）', () {
    /// 面板要连着一条会话才有 cwd（shell 的目录跟着工作区绑定走），
    /// 而它自己不建会话 —— 所以这里替它建一条草稿。
    Widget panelApp({String? agentOrigin}) => ProviderScope(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        localAgentOriginProvider.overrideWith(
          (ref) => Future.value(agentOrigin),
        ),
      ],
      child: const MaterialApp(home: Scaffold(body: TerminalPanel())),
    );

    testWidgets('本机 agent 没起来：不摆交互终端，退回只读命令记录', (tester) async {
      await tester.pumpWidget(panelApp());
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
      expect(
        find.byTooltip('新建终端'),
        findsNothing,
        reason: '连不上时那个加号点了什么都不会发生 —— 摆出来就是又一次撒谎',
      );
    });

    testWidgets('agent 活着且有会话：换成真的交互终端', (tester) async {
      await tester.pumpWidget(panelApp(agentOrigin: 'http://127.0.0.1:9'));
      await _settle(tester);

      final c = ProviderScope.containerOf(
        tester.element(find.byType(TerminalPanel)),
      );
      c.read(chatControllerProvider.notifier).createSession();
      // 面板不自己开标签 —— 顶栏那个按钮（或 Ctrl+`）才开
      c.read(terminalPanelProvider.notifier).addTab();
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

    /// ⚠️ **切标签不能把没在前面的那个 shell 弄没。**
    ///
    /// 每个标签背后是一个真的 shell 子进程，部件一销毁 WS 就关、agent
    /// 那侧就收掉它。写成「只画当前那个」的话，症状是「切到终端 2 再切
    /// 回来，终端 1 里跑着的东西没了」—— 而且没有任何报错。
    testWidgets('开两个标签，两个终端都留在树上', (tester) async {
      await tester.pumpWidget(panelApp(agentOrigin: 'http://127.0.0.1:9'));
      await _settle(tester);

      final c = ProviderScope.containerOf(
        tester.element(find.byType(TerminalPanel)),
      );
      c.read(chatControllerProvider.notifier).createSession();
      final panel = c.read(terminalPanelProvider.notifier);
      panel.addTab();
      panel.addTab();
      await _settle(tester);

      expect(
        find.byType(InteractiveTerminal, skipOffstage: false),
        findsNWidgets(2),
        reason: '第二个标签一开，第一个就被销毁了 —— 它的 shell 随之被收掉',
      );
      expect(find.text('终端 1'), findsOneWidget);
      expect(find.text('终端 2'), findsOneWidget);
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

  /// 终端那一块的底色。
  ///
  /// # 为什么这值得一条测试
  ///
  /// xterm 的 `TerminalThemes.defaultTheme` 是**写死的 VS Code 深色**。
  /// 不传 `theme:` 编译得过、跑得起来、字也看得见 —— 只是浅色主题下右栏里
  /// 挖出一块黑，而旁边两个页签（文件 / 本轮改动）跟着主题走。
  /// 这类「跑得起来但看着像坏了」的东西没有任何自动信号，只能钉住。
  group('终端跟着主题走', () {
    test('浅色主题下底色是这一栏的底色，不是那块写死的黑', () {
      final light = CortexTheme.light();
      final t = terminalTheme(light);

      expect(
        t.background,
        light.cortex.sidebar,
        reason: '终端住在右栏里，底色不一致就是一块没对齐的方块',
      );
      expect(
        t.background,
        isNot(const Color(0xFF1E1E1E)),
        reason: 'xterm 默认那块深灰 —— 这条断言就是冲着「忘了传 theme:」来的',
      );
      expect(
        t.foreground,
        light.colorScheme.onSurface,
        reason: '底色跟着主题走了而字没跟，就是浅底上的浅灰字，更糟',
      );
    });

    test('深色主题下也跟着走', () {
      final dark = CortexTheme.dark();
      expect(terminalTheme(dark).background, dark.cortex.sidebar);
    });

    /// 16 色**不**跟着主题算：它们是协议规定的语义（`ls` 绿标可执行、
    /// `git` 红标删除）。按强调色算出来的「绿」会让 `git diff` 读不懂。
    test('16 色只换深浅两套，不按主题色算', () {
      expect(
        terminalTheme(CortexTheme.light()).green,
        isNot(terminalTheme(CortexTheme.dark()).green),
        reason: '浅底上要用深一档的绿，否则 ls 的目录名糊在背景里',
      );
      expect(
        terminalTheme(CortexTheme.light()).red,
        terminalTheme(CortexTheme.dark()).red,
        reason: '红在两套里本来就同一个值 —— 这里钉的是「它不跟着 primary 变」',
      );
    });
  });

  /// **终端要真的能敲进去。**
  ///
  /// # 这条测试为什么必须存在
  ///
  /// 敲不进去这件事有一个**极具误导性的症状**：终端画得好好的、shell 的
  /// 首屏在流、resize 也生效（说明 WS 双向都通），只有键盘没反应。看起来
  /// 像焦点问题，而实际是 xterm 默认把可打印字符交给**平台文本输入通道**
  /// （为手机软键盘设计的那条），`_handleKeyEvent` 对普通字母一律回
  /// `ignored`。桌面上那条通道不出字。
  ///
  /// 我按「焦点」猜过一轮，改完仍然敲不进去 —— 一次白跑的实机验证。
  /// 这条测试把判据从「看起来对不对」变成「字节有没有出来」。
  ///
  /// 它直接驱动 `TerminalView`（不是整条右栏）：这里要钉的是**键怎么走到
  /// 终端**，而 WS、PTY、会话那几层各有自己的测试，混进来只会让这条在
  /// 别处改动时变红。
  group('终端敲得进字', () {
    /// 起一个只连着 `onOutput` 的终端，返回它吐出去的字节。
    Future<List<String>> pumpTerminal(WidgetTester tester) async {
      final out = <String>[];
      final terminal = Terminal()..onOutput = out.add;
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: TerminalView(
              terminal,
              autofocus: true,
              hardwareKeyboardOnly: true,
              theme: terminalTheme(CortexTheme.light()),
            ),
          ),
        ),
      );
      await tester.pump();
      return out;
    }

    testWidgets('敲一个普通字母，它出现在发给 shell 的字节里', (tester) async {
      final out = await pumpTerminal(tester);

      await tester.sendKeyEvent(LogicalKeyboardKey.keyL);
      await tester.sendKeyEvent(LogicalKeyboardKey.keyS);
      await tester.pump();

      expect(
        out.join(),
        'ls',
        reason:
            '普通字母没被送出去 —— 这正是「终端画出来了、输出在流、'
            '但敲什么都没反应」那个症状。默认那条路要平台文本输入通道给字，'
            '而桌面上它不给',
      );
    });

    testWidgets('回车与控制键也要走同一条路出去', (tester) async {
      final out = await pumpTerminal(tester);

      await tester.sendKeyEvent(LogicalKeyboardKey.enter);
      await tester.pump();
      expect(out.join(), '\r', reason: '回车没发出去的话，命令永远不会被执行');

      out.clear();
      await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
      await tester.sendKeyEvent(LogicalKeyboardKey.keyC);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
      await tester.pump();
      expect(out.join(), '\x03', reason: 'Ctrl-C 是终端里最要紧的一个键 —— 跑飞了的命令只能靠它停下');
    });

    /// ⚠️ **上面两条钉的是判据，这一条钉的是接线。**
    ///
    /// 它们自己造 `TerminalView`，所以把 `InteractiveTerminal` 里那一行
    /// 删掉照样全绿 —— 而那一行正是修复本身。同一个「函数对、接线断」的
    /// 形状这个仓库已经吃过三次。
    testWidgets('右栏那个终端真的用了这条路，不只是测试里用了', (tester) async {
      await tester.pumpWidget(
        const MaterialApp(
          home: Scaffold(
            body: InteractiveTerminal(
              // 连不上的地址：这条用例只看**建出来的部件长什么样**，
              // 而那一帧在连接失败之前就画出来了
              origin: 'http://127.0.0.1:1',
              sessionId: '01M0SESSIONAAAAAAAAAAAAAAA',
            ),
          ),
        ),
      );
      await tester.pump();

      final view = tester.widget<TerminalView>(find.byType(TerminalView));
      expect(
        view.hardwareKeyboardOnly,
        isTrue,
        reason: '右栏的终端没走硬件键盘那条路 —— 那就是一个字都敲不进去的终端',
      );
    });
  });
}
