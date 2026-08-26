import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import '../../core/attention.dart';
import '../../core/theme.dart';

import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../../state/terminal_panel.dart';
import '../chat/chat_pane.dart';
import '../assistants/assistants_page.dart';
import '../library/library_page.dart';
import '../images/image_page.dart';
import '../projects/projects_page.dart';
import '../sessions/session_list.dart';
import '../workspace/right_rail.dart';
import '../workspace/terminal_panel.dart';
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

  /// 抽屉形态的左栏宽度。
  ///
  /// **不跟着 `layout.leftWidth` 走**：那个是用户为「三栏并排」这个布局
  /// 调的，而抽屉是盖在内容上的另一回事 —— 窄屏上把它拖到 380 会盖掉
  /// 整个窗口。这里给一个固定的、放得下会话标题的宽度。
  static const _drawerWidth = 284.0;
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
    final term = ref.watch(terminalPanelProvider);
    // 有标签就把浮层挂上（哪怕此刻收着）—— 挂着才活得下去，见 `_terminalLayer`
    final terminals = term.tabs.isNotEmpty;

    return LayoutBuilder(
      builder: (context, constraints) {
        final width = constraints.maxWidth;
        final isWide = width >= kRailInlineBreakpoint;
        final isMedium = width >= _mediumBreakpoint;

        // 够宽**并且**没被收起来才内联。两个条件分工不同：断点说的是
        // 「放得下吗」，收起说的是「想不想要」，谁也不该替对方回答
        final showSessionsInline = isMedium && !layout.leftCollapsed;
        final showRightInline = isWide && layout.rightPanel != null;

        final leftWidth = layout.leftWidth;
        // 右栏能拉到多宽由**这一刻的窗口**说了算，所以在这儿算而不是在
        // notifier 里：把窗口拖窄再拖回来，右栏该回到原来那么宽 ——
        // 存进去的值被小窗口截过一次就再也回不来了
        final rightMax =
            (width - (showSessionsInline ? leftWidth : 0) - kChatPaneMin).clamp(
              kRightPaneMin,
              double.infinity,
            );
        final rightWidth = layout.rightWidth.clamp(kRightPaneMin, rightMax);

        // 终端此刻在不在右侧那一列上。
        final terminalUp =
            isWide && layout.rightPanel == RightPanel.terminal && terminals;
        // 铺满 = 盖住对话与右栏，但**不盖左侧栏**（那是导航，盖掉就没法
        // 换会话了）。用户的原话：「完全扩展到左侧侧边栏右侧全屏」
        final terminalFull = terminalUp && term.expanded;

        // 终端开关。**宽窄两条路语义不同**，所以在这儿分：
        // 宽屏它是右侧那一列的一位住客；窄屏那一列是从底部升起的一张纸，
        // 那里没有「铺满」这回事（它本来就占着 86% 的高度）
        void toggleTerminal() {
          if (isWide) {
            ref.read(terminalPanelProvider.notifier).toggle();
            return;
          }
          ref.read(terminalPanelProvider.notifier).ensureTab();
          layoutNotifier.showRight(RightPanel.terminal);
          showRightRailSheet(context);
        }

        return CallbackShortcuts(
          // ⌘K / Ctrl+K 呼出命令面板。挂在 shell 最外层：焦点在哪
          // （输入框、列表、设置页）都够得着 —— 命令面板的意义就是
          // 「不管在哪，两个键到任何地方」
          bindings: {
            const SingleActivator(LogicalKeyboardKey.keyK, control: true): () =>
                showCommandPalette(context),
            const SingleActivator(LogicalKeyboardKey.keyK, meta: true): () =>
                showCommandPalette(context),
            // Ctrl+` / ⌘` —— 终端。与 VS Code、Cursor、Claude Code 一致：
            // 这个键位在开发者手里已经是肌肉记忆，换一个只会让人先试它
            const SingleActivator(
              LogicalKeyboardKey.backquote,
              control: true,
            ): () =>
                toggleTerminal(),
            const SingleActivator(
              LogicalKeyboardKey.backquote,
              meta: true,
            ): () =>
                toggleTerminal(),
          },
          child: Scaffold(
            key: _scaffoldKey,
            drawer: showSessionsInline
                ? null
                : Drawer(
                    width: _drawerWidth,
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
              child: Stack(
                children: [
                  Row(
                    children: [
                      if (showSessionsInline) ...[
                        SizedBox(
                          width: leftWidth,
                          child: Container(
                            // 导航区是**独立表面**，不是「浅一点的内容区」。
                            // 抄 Cherry Studio 的 sidebar 那一档：两个功能区靠
                            // 底色分开，比靠一条线分开省力得多
                            color: tokens.sidebar,
                            child: const _LeftPane(collapsible: true),
                          ),
                        ),
                        _ResizeHandle(
                          color: tokens.sidebarBorder,
                          // 往右拖 = 左栏变宽。⚠️ 传的是**位移**不是算好的
                          // 宽度：一帧可能来好几次 update，而闭包里那个
                          // `leftWidth` 是 build 时的旧值 —— 见 nudgeLeftWidth
                          onDelta: layoutNotifier.nudgeLeftWidth,
                          onDone: layoutNotifier.persistWidths,
                        ),
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
                            activePanel: showRightInline
                                ? layout.rightPanel
                                : null,
                            onToggleTerminal: toggleTerminal,
                            terminalActive: terminalUp,
                          ),
                        },
                      ),
                      if (showRightInline) ...[
                        _ResizeHandle(
                          color: tokens.sidebarBorder,
                          // 往右拖 = 右栏变窄（在 nudge 里减）。同上：传位移
                          onDelta: (d) =>
                              layoutNotifier.nudgeRightWidth(d, rightMax),
                          onDone: layoutNotifier.persistWidths,
                        ),
                        SizedBox(
                          width: rightWidth,
                          child: Container(
                            color: tokens.sidebar,
                            // 终端在那一列上时这里**留白**：真正的终端画在下面
                            // 那层浮层里（理由见 `_terminalLayer`），这块只负责
                            // 把对话推开、别让它躲到终端底下
                            child: terminalUp
                                ? null
                                : RightRail(onClose: layoutNotifier.closeRight),
                          ),
                        ),
                      ],
                    ],
                  ),
                  // 终端浮层。见 `_terminalLayer`
                  if (terminals)
                    _terminalLayer(
                      tokens: tokens,
                      visible: terminalUp,
                      full: terminalFull,
                      leftInset: terminalFull && showSessionsInline
                          ? leftWidth + _kResizeHandleWidth
                          : null,
                      width: rightWidth,
                    ),
                ],
              ),
            ),
          ),
        );
      },
    );
  }

  /// 终端**始终画在浮层里**，位置在「右侧那一列」与「铺满」之间切。
  ///
  /// # 为什么不直接放进右侧那一列
  ///
  /// 因为它得**活下去**。每个标签背后是一个真的 shell 子进程，部件一销毁
  /// WS 就关、agent 那侧就收掉它。而右侧那一列在三种很常见的操作下会整个
  /// 消失或换人：切到「文件」页、收起右栏、点「铺满」。放在列里的话，
  /// 这三下每一下都会把跑着的 `npm run dev` 静默杀掉。
  ///
  /// 浮层里它只换约束、不换父节点，所以状态一直在。收起时用 `Offstage`
  /// 藏起来 —— 藏起来的部件不布局不绘制，但**元素树还在**，shell 照跑。
  ///
  /// # 为什么不用 `GlobalKey` 在两个父节点之间搬
  ///
  /// 那也能保住状态，但每一次搬家都要走一遍「卸载再挂载」的时序，而
  /// `TerminalView` 在那一瞬间会拿到零尺寸并据此发一条 resize —— 那条
  /// 消息会把用户屏幕上的输出折成一列。约束不变的浮层没有这个问题。
  Widget _terminalLayer({
    required CortexTokens tokens,
    required bool visible,
    required bool full,
    required double? leftInset,
    required double width,
  }) => Positioned(
    top: 0,
    bottom: 0,
    right: 0,
    left: full ? (leftInset ?? 0) : null,
    width: full ? null : width,
    child: Offstage(
      offstage: !visible,
      child: Container(
        color: tokens.sidebar,
        // 铺满时它盖在对话上面，需要一条边把两者分开；贴着右栏时
        // 左边那根分隔条已经在 Row 里画过了
        child: full
            ? Row(
                children: [
                  VerticalDivider(width: 1, color: tokens.sidebarBorder),
                  const Expanded(child: TerminalPanel()),
                ],
              )
            : const TerminalPanel(),
      ),
    ),
  );
}

/// `_ResizeHandle` 的命中区宽度。浮层要按它把自己往左让一让，
/// 不然铺满时会盖住那根拖动条。
const double _kResizeHandleWidth = 9;

/// 两栏之间那根**能拖的**分隔线。
///
/// # 画 1 像素，但能抓 9 像素
///
/// 分隔线在视觉上必须细（它是两块内容之间的界，不是第三块内容），而
/// 1 像素的命中区在任何显示器上都是抓不住的 —— 用户会以为它不能拖。
/// 所以命中区是 [_hit] 宽的透明块，线画在正中。这一条是所有可拖分隔条
/// 的通行做法，VS Code / Slack / Cherry Studio 都是这么干的。
///
/// # 为什么不用 `Draggable`
///
/// 那一套是「拎起来放到别处」，会画拖影、要有放置目标。这里要的是
/// 「跟着指针走」，`GestureDetector` 的 `onHorizontalDragUpdate` 正好。
///
/// # 拖动时不写盘
///
/// [onDelta] 每帧都会被叫，[onDone] 只在松手时叫一次。宽度存在设置里，
/// 每帧写一次等于一次拖拽几十次磁盘写 —— 而中间那些值没有一个是用户
/// 想留下的。
class _ResizeHandle extends StatefulWidget {
  const _ResizeHandle({
    required this.color,
    required this.onDelta,
    required this.onDone,
  });

  final Color color;

  /// 指针横向移动了多少（逻辑像素，向右为正）。
  final void Function(double dx) onDelta;

  /// 松手了 —— 该把最终宽度存下来。
  final VoidCallback onDone;

  @override
  State<_ResizeHandle> createState() => _ResizeHandleState();
}

class _ResizeHandleState extends State<_ResizeHandle> {
  static const double _hit = _kResizeHandleWidth;

  /// 悬停或正在拖时把线描粗一点。
  ///
  /// 这是这个控件唯一的「我能拖」信号 —— 鼠标指针的变化在触屏上没有，
  /// 而且它出现在指针**已经**移过去之后。一条会变化的线让人在移过去的
  /// 途中就看得出这儿有东西。
  bool _hot = false;

  @override
  Widget build(BuildContext context) {
    return MouseRegion(
      cursor: SystemMouseCursors.resizeLeftRight,
      onEnter: (_) => setState(() => _hot = true),
      onExit: (_) => setState(() => _hot = false),
      child: GestureDetector(
        // 透明也要吃点击：不然拖拽落到底下那栏上，表现是「拖不动，
        // 反而选中了一行会话」
        behavior: HitTestBehavior.opaque,
        onHorizontalDragUpdate: (d) => widget.onDelta(d.delta.dx),
        onHorizontalDragEnd: (_) => widget.onDone(),
        // 拖到窗口外面松手也算松手 —— 少了这一条，那次拖拽的宽度不会被存下
        onHorizontalDragCancel: widget.onDone,
        child: SizedBox(
          width: _hit,
          child: Center(
            child: AnimatedContainer(
              duration: const Duration(milliseconds: 120),
              width: _hot ? 3 : 1,
              color: widget.color,
            ),
          ),
        ),
      ),
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
