/// 右栏 —— 两页签：文件 / 本轮改动。
///
/// # 为什么从「只放文件」改成页签
///
/// 长 diff 从前只能在气泡里看（会把对话挤成一条缝）或点开一个弹层
/// （挡住对话）。改动是**看着对话对照读**的东西 —— 它需要的是与对话
/// 并排的一列，而右栏正好就是那一列。
///
/// 两者共用一个入口图标（顶栏的右栏图标），页签在栏内切：用户的心智是
/// 「打开右栏，然后在里面翻」，不是两个独立的栏 —— 它们回答的是同一类
/// 问题（这个会话动了哪些文件）。页签状态在 [railTabProvider] ——
/// 顶栏「本会话改动」与 diff 卡的「在右栏打开」都要能从外面切过来。
///
/// # 终端去哪了
///
/// 它 2026-08-26 从这里搬走了，成了右侧那一列的另一位住客
/// （`RightPanel.terminal`，见 `TerminalPanel`）。理由是它不是「看」的
/// 东西：终端要自己的标签页、开关、快捷键、还要能铺满 —— 这四样在一条
/// 共用的页签条里一样都摆不下。
///
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/theme.dart';
import '../../state/app_providers.dart';
import '../chat/session_changes_sheet.dart';
import 'terminal_panel.dart';
import 'workspace_panel.dart';

/// 右栏能内联的最小窗宽。`AppShell` 的三栏断点与「窄屏走底部抽屉」
/// 共用这一个数 —— 两处各写 1240 的话，迟早一处改了另一处没跟上，
/// 症状是某个宽度区间里点「在右栏打开」什么都不发生。
const double kRailInlineBreakpoint = 1240;

/// 右栏开关的图标 —— 顶栏（开/关）与栏头（收起）**必须是同一个**。
///
/// 常量放在右栏自己这儿、两处引用，而不是各写一遍：这个开关的语义是
/// 「侧栏折叠」（与左栏那对 menu_open 一致），两处图标一旦分叉，用户就
/// 认不出「栏头那个按钮收起去的，就是顶栏那个按钮能叫回来的」。
/// filled = 右栏开着，outlined = 收着 —— 与顶栏其余开关同一套激活语义。
const IconData kRailToggleFilled = Icons.view_sidebar_rounded;
const IconData kRailToggleOutlined = Icons.view_sidebar_outlined;

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
      child: Consumer(
        builder: (ctx, ref, _) =>
            ref.watch(layoutProvider).rightPanel == RightPanel.terminal
            // ⚠️ 窄屏上终端的 shell **活不过这张纸**：它挂在弹层里，
            // 纸一收部件就销毁，WS 随之关闭。宽屏那条路不是这样
            // （见 `AppShell._terminalLayer`）。
            // 不做成一样是因为弹层没有「留在树上」的位置，而为它专门
            // 造一个，代价是窄屏上多一块永远挂着的隐藏区域
            ? const TerminalPanel(canExpand: false)
            : RightRail(onClose: () => Navigator.of(sheetContext).pop()),
      ),
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
              // ✕ 改成了折叠语义：这一栏不是弹层，是与左栏对称的一列。
              // 「关闭」暗示它没了，而它只是收起去了 —— 顶栏保留同一个
              // 图标随时展开（窄屏的底部抽屉形态下，这里收的是抽屉本身，
              // 语义同样成立：再点顶栏那个图标它就回来）
              if (onClose != null)
                IconButton(
                  key: const ValueKey('rail:collapse'),
                  onPressed: onClose,
                  iconSize: 17,
                  tooltip: '收起右栏',
                  icon: const Icon(kRailToggleOutlined),
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
