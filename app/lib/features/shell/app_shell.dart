import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../chat/chat_pane.dart';
import '../memory/memory_panel.dart';
import '../sessions/session_list.dart';
import '../settings/settings_sheet.dart';

/// Responsive three-pane shell.
///
/// Breakpoints are driven by content, not device class — there is no "is this
/// mobile" check anywhere, which is what lets the same tree serve a Windows
/// window being dragged narrow and a browser tab at any size.
///
/// * `>= 1240` — sessions | chat | memory, all resident.
/// * `900–1240` — sessions | chat; memory moves to an end drawer.
/// * `< 900` — chat only; both side panes become drawers.
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
                    child: SessionList(
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
                      child: const SessionList(),
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
