/// 右栏 —— 三页签：文件 / 本轮改动 / 终端。
///
/// # 为什么从「只放文件」改成页签
///
/// 长 diff 从前只能在气泡里看（会把对话挤成一条缝）或点开一个弹层
/// （挡住对话）。「改动」与「终端输出」都是**看着对话对照读**的东西 ——
/// 它们需要的是与对话并排的一列，而右栏正好就是那一列。
///
/// 三者共用一个入口图标（顶栏的文件图标），页签在栏内切：用户的心智是
/// 「打开右栏，然后在里面翻」，不是三个独立的栏。页签状态在
/// [railTabProvider] —— 顶栏「本会话改动」与 diff 卡的「在右栏打开」
/// 都要能从外面切过来。
///
/// # 「终端」页签是什么
///
/// 这一轮里跑过的每条 shell 命令与它的输出，按时间顺序。不是一个能敲的
/// 终端 —— 敲命令是模型的事（或者用户自己开真终端）；这里回答的是
/// 「它刚才到底跑了什么、输出了什么」，长输出在气泡里同样会挤成缝。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/theme.dart';
import '../../models/tool_call.dart';
import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../chat/session_changes_sheet.dart';
import 'workspace_panel.dart';

/// 右栏能内联的最小窗宽。`AppShell` 的三栏断点与「窄屏走底部抽屉」
/// 共用这一个数 —— 两处各写 1240 的话，迟早一处改了另一处没跟上，
/// 症状是某个宽度区间里点「在右栏打开」什么都不发生。
const double kRailInlineBreakpoint = 1240;

/// 窄屏上把右栏当**底部抽屉**拉上来。
///
/// # 为什么从底部，不从右边
///
/// 从右边推的面板与系统手势打架：Android 的返回手势、iPad 的边缘滑动、
/// Windows 触屏的右缘呼出，全都占着右边缘。底部这条边只有键盘用，
/// 而这个面板弹出时没人在打字。
Future<void> showRightRailSheet(BuildContext context) {
  final tokens = Theme.of(context).cortex;
  return showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    useSafeArea: true,
    backgroundColor: tokens.sidebar,
    shape: const RoundedRectangleBorder(
      borderRadius: BorderRadius.vertical(
        top: Radius.circular(CortexTokens.radiusWindow),
      ),
    ),
    builder: (sheetContext) => FractionallySizedBox(
      // 86%：顶上留一条看得见对话的缝 —— 全屏会让人忘了自己从哪来
      heightFactor: 0.86,
      child: RightRail(onClose: () => Navigator.of(sheetContext).pop()),
    ),
  );
}

class RightRail extends ConsumerWidget {
  const RightRail({super.key, this.onClose});

  final VoidCallback? onClose;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final tokens = theme.cortex;
    final tab = ref.watch(railTabProvider);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(10, 8, 6, 4),
          child: Row(
            children: [
              for (final t in RailTab.values) ...[
                _TabChip(
                  label: t.label,
                  selected: t == tab,
                  onTap: () => ref.read(railTabProvider.notifier).go(t),
                ),
                const SizedBox(width: 4),
              ],
              const Spacer(),
              if (onClose != null)
                IconButton(
                  onPressed: onClose,
                  iconSize: 17,
                  tooltip: '关闭',
                  icon: const Icon(Icons.close_rounded),
                ),
            ],
          ),
        ),
        Divider(height: 1, color: tokens.sidebarBorder),
        Expanded(
          child: switch (tab) {
            // 文件页签复用整个 WorkspacePanel（它自己的 header 里有
            // 「更换工作区」）。onClose 不传 —— 关闭按钮在页签条上，
            // 传了就是两个 ✕ 管同一件事
            RailTab.files => const WorkspacePanel(),
            RailTab.changes => const Padding(
              padding: EdgeInsets.fromLTRB(12, 8, 12, 8),
              child: SessionChangesView(),
            ),
            RailTab.terminal => const _TerminalView(),
          },
        ),
      ],
    );
  }
}

class _TabChip extends StatelessWidget {
  const _TabChip({
    required this.label,
    required this.selected,
    required this.onTap,
  });

  final String label;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final tokens = theme.cortex;
    return Material(
      color: selected ? tokens.sidebarAccent : Colors.transparent,
      borderRadius: BorderRadius.circular(CortexTokens.radiusRow),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(CortexTokens.radiusRow),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
          child: Text(
            label,
            style: theme.textTheme.labelMedium?.copyWith(
              fontWeight: selected ? FontWeight.w600 : null,
              color: selected
                  ? theme.colorScheme.onSurface
                  : tokens.foregroundTertiary,
            ),
          ),
        ),
      ),
    );
  }
}

/// 这一轮跑过的 shell 命令与输出，按时间顺序。
class _TerminalView extends ConsumerWidget {
  const _TerminalView();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final messages = ref.watch(
      chatControllerProvider.select((s) => s.activeTranscript),
    );
    final calls = [
      for (final m in messages)
        for (final ToolCall c in m.toolCalls)
          if (c.name == 'shell') c,
    ];

    if (calls.isEmpty) {
      return Padding(
        padding: const EdgeInsets.all(16),
        child: Text(
          '这个会话还没有跑过命令。',
          style: theme.textTheme.bodyMedium?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
        ),
      );
    }

    return ListView.builder(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 12),
      itemCount: calls.length,
      itemBuilder: (context, i) {
        final c = calls[i];
        return Padding(
          padding: const EdgeInsets.only(bottom: 10),
          child: Container(
            decoration: BoxDecoration(
              color: scheme.surfaceContainerHigh,
              borderRadius: BorderRadius.circular(CortexTokens.radiusRow),
              border: Border.all(color: scheme.outlineVariant),
            ),
            padding: const EdgeInsets.all(10),
            child: SelectionArea(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        r'$ ',
                        style: theme.textTheme.bodySmall?.copyWith(
                          fontFamily: 'monospace',
                          color: theme.cortex.foregroundTertiary,
                        ),
                      ),
                      Expanded(
                        child: Text(
                          c.arguments ?? '(命令内容不可用)',
                          style: theme.textTheme.bodySmall?.copyWith(
                            fontFamily: 'monospace',
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                      if (c.failed)
                        Text(
                          '失败',
                          style: theme.textTheme.labelSmall?.copyWith(
                            color: scheme.error,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                    ],
                  ),
                  if ((c.result ?? '').isNotEmpty) ...[
                    const SizedBox(height: 6),
                    Text(
                      c.result!,
                      style: theme.textTheme.bodySmall?.copyWith(
                        fontFamily: 'monospace',
                        height: 1.5,
                        color: scheme.onSurfaceVariant,
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
