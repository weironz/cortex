import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../chat/chat_pane.dart';
import '../memory/memory_panel.dart';
import '../sessions/session_list.dart';
import '../settings/settings_sheet.dart';
import '../workspace/workspace_panel.dart';

/// Responsive three-pane shell.
///
/// Breakpoints are driven by content, not device class — there is no "is this
/// mobile" check anywhere, which is what lets the same tree serve a Windows
/// window being dragged narrow and a browser tab at any size.
///
/// * `>= 1240` — sessions | chat | memory, all resident.
/// * `900–1240` — sessions | chat; memory moves to an end drawer.
/// * `< 900` — chat only; both side panes become drawers.
///
/// ## Why the file tree is not a fourth column
///
/// The workspace tree shares the left pane with the session list instead of
/// getting a column of its own. A fourth column would say, structurally, that
/// files are a peer of the conversation — a separate place you go. They are
/// not: a workspace is a *property of the session*, like the model it uses, and
/// the tree is there to answer "which directory is this pointed at". Putting it
/// under the sessions it belongs to keeps that relationship visible, and keeps
/// the widest layout at three columns instead of four.
class AppShell extends ConsumerStatefulWidget {
  const AppShell({super.key});

  @override
  ConsumerState<AppShell> createState() => _AppShellState();
}

class _AppShellState extends ConsumerState<AppShell> {
  final GlobalKey<ScaffoldState> _scaffoldKey = GlobalKey<ScaffoldState>();

  /// User-toggled visibility for the resident memory pane on wide layouts.
  bool _memoryPaneVisible = true;

  static const _sessionsWidth = 264.0;
  static const _memoryWidth = 348.0;
  static const _wideBreakpoint = 1240.0;
  static const _mediumBreakpoint = 900.0;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;

    return LayoutBuilder(
      builder: (context, constraints) {
        final width = constraints.maxWidth;
        final isWide = width >= _wideBreakpoint;
        final isMedium = width >= _mediumBreakpoint;

        final showSessionsInline = isMedium;
        final showMemoryInline = isWide && _memoryPaneVisible;

        return Scaffold(
          key: _scaffoldKey,
          drawer: showSessionsInline
              ? null
              : Drawer(
                  width: _sessionsWidth + 20,
                  backgroundColor: scheme.surfaceContainerLow,
                  child: SafeArea(
                    child: _LeftPane(
                      availableHeight: constraints.maxHeight,
                      onSelected: () => Navigator.of(context).maybePop(),
                    ),
                  ),
                ),
          endDrawer: showMemoryInline
              ? null
              : Drawer(
                  width: _memoryWidth + 20,
                  backgroundColor: scheme.surfaceContainerLow,
                  child: SafeArea(
                    child: MemoryPanel(
                      onClose: () => Navigator.of(context).maybePop(),
                    ),
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
                      child: _LeftPane(
                        availableHeight: constraints.maxHeight,
                      ),
                    ),
                  ),
                  VerticalDivider(width: 1, color: scheme.outlineVariant),
                ],
                Expanded(
                  child: ChatPane(
                    onOpenSessions: showSessionsInline
                        ? null
                        : () => _scaffoldKey.currentState?.openDrawer(),
                    onOpenSettings: () => showSettingsSheet(context),
                    onOpenMemory: isWide
                        ? () => setState(
                            () => _memoryPaneVisible = !_memoryPaneVisible,
                          )
                        : () => _scaffoldKey.currentState?.openEndDrawer(),
                  ),
                ),
                if (showMemoryInline) ...[
                  VerticalDivider(width: 1, color: scheme.outlineVariant),
                  SizedBox(
                    width: _memoryWidth,
                    child: Container(
                      color: scheme.surfaceContainerLow,
                      child: MemoryPanel(
                        onClose: () =>
                            setState(() => _memoryPaneVisible = false),
                      ),
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

/// Session list on top, workspace tree below.
///
/// The split is computed rather than flexed because both halves are scrollable:
/// two `Expanded` children would each claim the leftover space and the tree
/// would take half the pane even when the user has forty sessions and three
/// files. Capping the tree at 40% of the pane (and at 320px) keeps the session
/// list — the thing you navigate with — dominant.
class _LeftPane extends StatelessWidget {
  const _LeftPane({required this.availableHeight, this.onSelected});

  final double availableHeight;
  final VoidCallback? onSelected;

  @override
  Widget build(BuildContext context) {
    final treeHeight = (availableHeight * 0.4).clamp(160.0, 320.0);
    return Column(
      children: [
        Expanded(child: SessionList(onSelected: onSelected)),
        WorkspacePanel(maxTreeHeight: treeHeight),
      ],
    );
  }
}
