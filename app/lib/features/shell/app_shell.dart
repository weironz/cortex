import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../chat/chat_pane.dart';
import '../sessions/session_list.dart';
import '../workspace/workspace_panel.dart';
import 'widgets/account_bar.dart';

/// Responsive three-pane shell.
///
/// Breakpoints are driven by content, not device class — there is no "is this
/// mobile" check anywhere, which is what lets the same tree serve a Windows
/// window being dragged narrow and a browser tab at any size.
///
/// * `>= 1240` — sessions | chat | 右栏, all resident.
/// * `900–1240` — sessions | chat; 右栏 moves to an end drawer.
/// * `< 900` — chat only; both side panes become drawers.
///
/// ## 右侧只有一列，记忆与文件轮流住
///
/// 两者互斥不是妥协，是**它们回答的是同一类问题**：「除了这段对话，
/// 还有什么是属于我的」。同时摆两列会把最宽的布局推到四列，而第四列
/// 挤掉的正是对话本身。
///
/// 互斥由 [RightPanel] 这个可空枚举在**类型上**保证，不靠这里的判断 ——
/// 见它的文档。
///
/// ## 文件树曾经在左栏底下，为什么搬走了
///
/// 原来的论证是「工作区是**会话的一个属性**，像它用哪个模型一样，
/// 那棵树只是回答『这个会话指着哪个目录』」——所以它跟着会话列表放。
///
/// **那个前提已经不成立**：云端工作区改成按项目分、常驻、与会话绑定无关
/// （容器与卷按 `SandboxScope::key` 走），Web 端更是既绑不了也不需要绑。
/// 文件从「会话的一个属性」变成了「一个持续存在的地方」，而右栏正是
/// 放「地方」的位置。左栏底下那个限高 320px 的框也随之取消 ——
/// 整列之后，文件多的时候不再被那个高度卡住。
class AppShell extends ConsumerStatefulWidget {
  const AppShell({super.key});

  @override
  ConsumerState<AppShell> createState() => _AppShellState();
}

class _AppShellState extends ConsumerState<AppShell> {
  final GlobalKey<ScaffoldState> _scaffoldKey = GlobalKey<ScaffoldState>();

  static const _sessionsWidth = 264.0;
  static const _rightWidth = 348.0;
  static const _wideBreakpoint = 1240.0;
  static const _mediumBreakpoint = 900.0;

  @override
  void initState() {
    super.initState();
    // 「你不在的时候那一轮跑完了」。
    //
    // 挂在 shell 上而不是聊天面板上：面板会随着布局收起来（窄屏上它可能
    // 正被抽屉盖着），而这条提示恰恰是给「人不在那个会话上」的时候用的。
    //
    // 用 listenManual 而不是 build 里 watch：弹 SnackBar 是副作用，
    // 放 build 里会在每次无关重建时再弹一次
    ref.listenManual(chatControllerProvider.select((s) => s.finished), (
      was,
      now,
    ) {
      final fresh = now.difference(was ?? const <String>{});
      if (fresh.isEmpty || !mounted) return;
      final id = fresh.first;
      final sessions = ref.read(chatControllerProvider).sessions;
      final title = sessions
          .where((s) => s.id == id)
          .map((s) => s.title)
          .firstOrNull;
      final messenger = ScaffoldMessenger.maybeOf(context);
      if (messenger == null) return;
      messenger.showSnackBar(
        SnackBar(
          content: Text(title == null ? '有一轮跑完了' : '「$title」跑完了'),
          duration: const Duration(seconds: 6),
          action: SnackBarAction(
            label: '去看看',
            onPressed: () =>
                ref.read(chatControllerProvider.notifier).selectSession(id),
          ),
        ),
      );
    });
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final layout = ref.watch(layoutProvider);
    final layoutNotifier = ref.read(layoutProvider.notifier);

    return LayoutBuilder(
      builder: (context, constraints) {
        final width = constraints.maxWidth;
        final isWide = width >= _wideBreakpoint;
        final isMedium = width >= _mediumBreakpoint;

        // 够宽**并且**没被收起来才内联。两个条件分工不同：断点说的是
        // 「放得下吗」，收起说的是「想不想要」，谁也不该替对方回答
        final showSessionsInline = isMedium && !layout.leftCollapsed;
        final showRightInline = isWide && layout.rightPanel != null;

        // 右栏画谁。内联与抽屉共用它 —— 两处各写一遍 switch 的话，
        // 加第三个面板时漏改一处不会报错，只是抽屉里画的是另一个
        Widget rightPane(VoidCallback onClose) => switch (layout.rightPanel) {
          RightPanel.files || null => WorkspacePanel(onClose: onClose),
          // `null` 只会在抽屉那条路上出现：内联那条已被 `showRightInline`
          // 挡住，而点图标进来的走 `showRight`（它必定先设好再开抽屉）。
          // 剩下的唯一入口是**从屏幕右缘划开**抽屉 —— 那时用户没说要看哪个，
          // 而记忆是默认那个。画空白比画记忆更像「坏了」
        };

        return Scaffold(
          key: _scaffoldKey,
          drawer: showSessionsInline
              ? null
              : Drawer(
                  width: _sessionsWidth + 20,
                  backgroundColor: scheme.surfaceContainerLow,
                  child: SafeArea(
                    child: _LeftPane(
                      onSelected: () => Navigator.of(context).maybePop(),
                    ),
                  ),
                ),
          endDrawer: showRightInline
              ? null
              : Drawer(
                  width: _rightWidth + 20,
                  backgroundColor: scheme.surfaceContainerLow,
                  child: SafeArea(
                    child: rightPane(() => Navigator.of(context).maybePop()),
                  ),
                ),
          body: SafeArea(
            child: Row(
              children: [
                if (showSessionsInline) ...[
                  SizedBox(
                    width: _sessionsWidth,
                    child: Container(
                      color: scheme.surfaceContainerLow,
                      child: const _LeftPane(),
                    ),
                  ),
                  VerticalDivider(width: 1, color: scheme.outlineVariant),
                ],
                Expanded(
                  child: ChatPane(
                    // 同一个按钮、同一句话（「显示/隐藏会话」），但窄到放不下
                    // 内联时它只能开抽屉 —— 那时「收起」这个状态无处安放。
                    // 由这里按断点决定它具体做什么，ChatPane 只管画
                    onToggleSessions: isMedium
                        ? layoutNotifier.toggleLeft
                        : () => _scaffoldKey.currentState?.openDrawer(),
                    sessionsVisible: showSessionsInline,
                    // 两条路的语义不同，所以显式分开写：
                    //
                    // 宽屏：右栏就是 `rightPanel`，点同一个 = 收起。
                    // 窄屏：右栏是抽屉，开合归 `Scaffold` 管 —— 这里只能
                    //   「选中」。用 selectRight 的话，连点两次「文件」的
                    //   第二次会把它置空，而抽屉照样开，里面画的是记忆。
                    //   顺序也不能反：抽屉的内容取自 `layout.rightPanel`
                    onSelectPanel: (panel) {
                      if (isWide) {
                        layoutNotifier.selectRight(panel);
                      } else {
                        layoutNotifier.showRight(panel);
                        _scaffoldKey.currentState?.openEndDrawer();
                      }
                    },
                    activePanel: showRightInline ? layout.rightPanel : null,
                  ),
                ),
                if (showRightInline) ...[
                  VerticalDivider(width: 1, color: scheme.outlineVariant),
                  SizedBox(
                    width: _rightWidth,
                    child: Container(
                      color: scheme.surfaceContainerLow,
                      child: rightPane(layoutNotifier.closeRight),
                    ),
                  ),
                ],
              ],
            ),
          ),
        );
      },
    );
  }
}

/// 会话列表 + 底部账号栏。
///
/// 文件树原来夹在这两者之间，占着一个「40% 高、最多 320px」的框。搬到右栏
/// 之后这里只剩一件事，`Expanded` 就够了 —— 原来那套计算高度的理由
/// （两个可滚动的孩子不能都 `Expanded`）随之消失。
class _LeftPane extends StatelessWidget {
  const _LeftPane({this.onSelected});

  final VoidCallback? onSelected;

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        Expanded(child: SessionList(onSelected: onSelected)),
        // 内联与抽屉共用这一棵，所以账号栏两处都会有 —— 不必为抽屉
        // 单独再摆一遍，也就不会有两份各自漂移的版本
        const AccountBar(),
      ],
    );
  }
}
