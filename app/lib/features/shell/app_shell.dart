import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import '../../core/attention.dart';
import '../../core/theme.dart';

import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../chat/chat_pane.dart';
import '../assistants/assistants_page.dart';
import '../library/library_page.dart';
import '../images/image_page.dart';
import '../projects/projects_page.dart';
import '../sessions/session_list.dart';
import '../workspace/right_rail.dart';
import 'command_palette.dart';
import 'widgets/account_bar.dart';
import 'widgets/nav_block.dart';

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

      // 把话**送到窗口外面**去。下面那条 SnackBar 只有人正看着这扇窗
      // 时才看得见，而这个能力想省掉的事恰恰是「隔一会儿点进去看一眼」。
      //
      // 两侧都自带「已经在前台就不做」的判据（见 `attention.dart`），
      // 所以这里无条件调 —— 在这儿再判一次就是同一件事判两处，
      // 而两处迟早会不一致
      unawaited(
        callAttention(
          title == null ? 'Cortex' : '「$title」跑完了',
          title == null ? '有一轮跑完了' : '回来看看结果',
        ),
      );

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
    final tokens = Theme.of(context).cortex;
    final layout = ref.watch(layoutProvider);
    final layoutNotifier = ref.read(layoutProvider.notifier);

    return LayoutBuilder(
      builder: (context, constraints) {
        final width = constraints.maxWidth;
        final isWide = width >= kRailInlineBreakpoint;
        final isMedium = width >= _mediumBreakpoint;

        // 够宽**并且**没被收起来才内联。两个条件分工不同：断点说的是
        // 「放得下吗」，收起说的是「想不想要」，谁也不该替对方回答
        final showSessionsInline = isMedium && !layout.leftCollapsed;
        final showRightInline = isWide && layout.rightPanel != null;

        // 右栏画谁。内联与抽屉共用它 —— 两处各写一遍 switch 的话，
        // 加第三个面板时漏改一处不会报错，只是抽屉里画的是另一个
        // 2026-08-24 起右栏是 RightRail（文件 / 本轮改动 / 终端三页签），
        // WorkspacePanel 变成它的第一页。RightPanel 仍然只有一格 ——
        // 页签在栏内切，见 RailTab 上那段
        Widget rightPane(VoidCallback onClose) => switch (layout.rightPanel) {
          RightPanel.files || null => RightRail(onClose: onClose),
          // `null` 只会在抽屉那条路上出现：内联那条已被 `showRightInline`
          // 挡住，而点图标进来的走 `showRight`（它必定先设好再开抽屉）。
          // 剩下的唯一入口是**从屏幕右缘划开**抽屉 —— 那时用户没说要看哪个。
          // 画空白比画默认页更像「坏了」
        };

        return CallbackShortcuts(
          // ⌘K / Ctrl+K 呼出命令面板。挂在 shell 最外层：焦点在哪
          // （输入框、列表、设置页）都够得着 —— 命令面板的意义就是
          // 「不管在哪，两个键到任何地方」
          bindings: {
            const SingleActivator(LogicalKeyboardKey.keyK, control: true): () =>
                showCommandPalette(context),
            const SingleActivator(LogicalKeyboardKey.keyK, meta: true): () =>
                showCommandPalette(context),
          },
          child: Scaffold(
            key: _scaffoldKey,
            drawer: showSessionsInline
                ? null
                : Drawer(
                    width: _sessionsWidth + 20,
                    backgroundColor: tokens.sidebar,
                    child: SafeArea(
                      child: _LeftPane(
                        onSelected: () => Navigator.of(context).maybePop(),
                      ),
                    ),
                  ),
            // ⚠️ 刻意没有 endDrawer：窄屏的右栏从**底部**抽上来
            // （showRightRailSheet）。从右边推会和系统手势打架 ——
            // Android 返回、iPad 边缘滑动、Windows 触屏右缘呼出全占着那条边
            body: SafeArea(
              child: Row(
                children: [
                  if (showSessionsInline) ...[
                    SizedBox(
                      width: _sessionsWidth,
                      child: Container(
                        // 导航区是**独立表面**，不是「浅一点的内容区」。
                        // 抄 Cherry Studio 的 sidebar 那一档：两个功能区靠
                        // 底色分开，比靠一条线分开省力得多
                        color: tokens.sidebar,
                        child: const _LeftPane(collapsible: true),
                      ),
                    ),
                    VerticalDivider(width: 1, color: tokens.sidebarBorder),
                  ],
                  // 中间那一大栏是哪个「地方」。**只有聊天那一支要顶栏
                  // 那堆参数** —— 画廊没有会话、没有右栏面板，把它硬塞进
                  // 同一个组件里只会多出一串对它无意义的入参
                  Expanded(
                    child: switch (ref.watch(mainViewProvider)) {
                      MainView.library => LibraryPageView(
                        onToggleSessions: isMedium
                            ? layoutNotifier.toggleLeft
                            : () => _scaffoldKey.currentState?.openDrawer(),
                      ),
                      MainView.assistants => AssistantsPage(
                        onToggleSessions: isMedium
                            ? layoutNotifier.toggleLeft
                            : () => _scaffoldKey.currentState?.openDrawer(),
                        sessionsVisible: showSessionsInline,
                      ),
                      MainView.projects => ProjectsPage(
                        onToggleSessions: isMedium
                            ? layoutNotifier.toggleLeft
                            : () => _scaffoldKey.currentState?.openDrawer(),
                        sessionsVisible: showSessionsInline,
                      ),
                      MainView.images => ImagePage(
                        // 窄屏上左栏是抽屉，画廊里也要有路把它叫出来 ——
                        // 少了这个按钮，从画廊回会话列表就只剩「从屏幕左缘划」
                        onToggleSessions: isMedium
                            ? layoutNotifier.toggleLeft
                            : () => _scaffoldKey.currentState?.openDrawer(),
                        sessionsVisible: showSessionsInline,
                      ),
                      MainView.chat => ChatPane(
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
                            showRightRailSheet(context);
                          }
                        },
                        // 「带我去」语义：无论右栏此刻开没开，开着并停在
                        // 这个面板上。与上面开关语义的差别见 ChatPane 的注释
                        onOpenPanel: (panel) {
                          layoutNotifier.showRight(panel);
                          if (!isWide) showRightRailSheet(context);
                        },
                        activePanel: showRightInline ? layout.rightPanel : null,
                      ),
                    },
                  ),
                  if (showRightInline) ...[
                    VerticalDivider(width: 1, color: tokens.sidebarBorder),
                    SizedBox(
                      width: _rightWidth,
                      child: Container(
                        color: tokens.sidebar,
                        child: rightPane(layoutNotifier.closeRight),
                      ),
                    ),
                  ],
                ],
              ),
            ),
          ),
        );
      },
    );
  }
}

/// 侧栏最上面那一行 —— 目前只有收起按钮。
///
/// 单独一个部件而不是几行内联：窄屏上侧栏是抽屉，那里**不该有**这个按钮
/// （抽屉自己会关，一个「收起」在那儿收的是什么并不清楚）。有个部件才好
/// 让那条路自己判断。
class _SidebarTopRow extends ConsumerWidget {
  const _SidebarTopRow();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 6, 8, 0),
      child: Row(
        children: [
          IconButton(
            key: const ValueKey('sidebar:collapse'),
            onPressed: ref.read(layoutProvider.notifier).toggleLeft,
            iconSize: 18,
            visualDensity: VisualDensity.compact,
            tooltip: '收起侧栏',
            icon: const Icon(Icons.menu_open_rounded),
          ),
        ],
      ),
    );
  }
}

/// 导航 + 会话列表 + 底部账号栏。
///
/// 文件树原来夹在这两者之间，占着一个「40% 高、最多 320px」的框。搬到右栏
/// 之后这里只剩一件事，`Expanded` 就够了 —— 原来那套计算高度的理由
/// （两个可滚动的孩子不能都 `Expanded`）随之消失。
///
/// 顶上那一小块导航是 2026-08-22 加画廊时进来的，见 [NavBlock] 的文档。
class _LeftPane extends ConsumerWidget {
  const _LeftPane({this.onSelected, this.collapsible = false});

  final VoidCallback? onSelected;

  /// 画不画「收起侧栏」那个按钮。
  ///
  /// **显式一个参数，而不是拿 `onSelected == null` 当判据**：那个回调回答的
  /// 是「选完要不要关抽屉」，与「这里收得起来吗」是两件事。借它当判据的话，
  /// 哪天内联那条路也想要一个 onSelected，按钮就无声地消失了。
  final bool collapsible;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    // **点会话就回聊天。** 人在画廊里点一条会话，要的是去看那条会话 ——
    // 停在原地的表现是「点了没反应」，而那正是这一轮刚修过的那类 bug。
    //
    // 挂在这里而不是 `ChatController.selectSession` 里：控制器不该知道
    // 界面上有几个「地方」，而 `onSelected` 恰好在每一次选中/新建时都会响
    void selected() {
      ref.read(mainViewProvider.notifier).go(MainView.chat);
      onSelected?.call();
    }

    return Column(
      children: [
        // ⚠️ **收起按钮长在侧栏自己身上。**
        //
        // 从前它只在内容区的顶栏上（`ChatPane` / `ImagePage` 的
        // `onToggleSessions`）。那个位置回答的是「把它叫回来」，而
        // 「把它收起去」的手该落在**要收的那个东西**上 —— lobehub 与
        // claude.ai 都把它放在侧栏左上角。
        //
        // 内容区那个**保留**：侧栏一收起，它自己的按钮也跟着没了，
        // 那时唯一的回路就是内容区那个。两者调的是同一个 `toggleLeft`，
        // 不是两套状态。
        if (collapsible) const _SidebarTopRow(),
        NavBlock(onNavigated: onSelected),
        Expanded(child: SessionList(onSelected: selected)),
        // 内联与抽屉共用这一棵，所以账号栏两处都会有 —— 不必为抽屉
        // 单独再摆一遍，也就不会有两份各自漂移的版本
        const AccountBar(),
      ],
    );
  }
}
