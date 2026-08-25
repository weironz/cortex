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
/// **能连上本机 agent 时，是一个真的能敲的终端**（PTY + xterm，见
/// [InteractiveTerminal]）。连不上（Web、或桌面端 agent 没起来）时退回
/// 只读的命令记录：这一轮里跑过的每条 shell 命令与它的输出，按时间顺序 ——
/// 那是「它刚才到底跑了什么」的答案，在没有本地 shell 的构建上依然成立。
/// 判据在 [_TerminalTab] 一处；两种形态都不撒谎（约束 2）。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/ansi.dart';
import '../../core/terminal_channel.dart';
import '../../core/theme.dart';
import '../../models/tool_call.dart';
import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../chat/session_changes_sheet.dart';
import 'interactive_terminal.dart';
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
            RailTab.terminal => const _TerminalTab(),
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

/// 「终端」页签画哪一种 —— **判据只在这里一处**。
///
/// 交互终端要三样都在：这个构建连得了（`kInteractiveTerminalSupported`，
/// Web 恒 false）、本机 agent 活着（origin 非 null —— 死了的 agent 上
/// 摆一个连不上的终端就是约束 2 说的那种谎）、有选中的会话（shell 的
/// cwd 跟着会话的工作区绑定走）。缺任何一样退回只读的命令记录 ——
/// 那在所有构建上都是真的。
///
/// 按 session 换 key：切会话就销毁重建，旧 shell 随 WS 关闭被收掉，
/// 新会话拿到自己工作区里的新 shell —— 「生命周期跟着会话走」全在这一个
/// key 上，不另设状态。
class _TerminalTab extends ConsumerWidget {
  const _TerminalTab();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final origin = ref.watch(localAgentOriginProvider).value;
    final sessionId = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    if (!kInteractiveTerminalSupported || origin == null || sessionId == null) {
      return const _TerminalView();
    }
    return InteractiveTerminal(
      key: ValueKey('terminal:$sessionId'),
      origin: origin,
      sessionId: sessionId,
      // read 而不是 watch：这把凭据钉死在 agent 启动那一刻，agent 换代时
      // origin 必然跟着变，上面那个 watch 已经会重建这里
      token: ref.read(localAgentHandleProvider).pinnedCredential,
    );
  }
}

/// 这一轮跑过的 shell 命令与输出，按时间顺序 —— 没有本地 shell 时的形态。
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
                          fontFamily: 'JetBrains Mono',
                          fontFamilyFallback: CortexTheme.monoFallback,
                          color: theme.cortex.foregroundTertiary,
                        ),
                      ),
                      Expanded(
                        child: Text(
                          // shellCommand 而不是 arguments：后者是 daemon 的
                          // `(command=…)` 包装，接在 `$ ` 后面画出来是
                          // `$ (command=git status)` —— 伪终端的形式感
                          // 反而放大了内容的不对
                          c.shellCommand ?? '(命令内容不可用)',
                          style: theme.textTheme.bodySmall?.copyWith(
                            fontFamily: 'JetBrains Mono',
                            fontFamilyFallback: CortexTheme.monoFallback,
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
                    // 输出走 ANSI 解析着色：这里放的是真终端的输出，
                    // cargo / npm / git 一律带色，不解析的话用户看到的是
                    // `[32m通过[0m` 而不是一个绿色的「通过」。失败时不解析
                    // —— 整段错误色说的是「这条挂了」，让命令自己的配色
                    // 盖过它只会把这件事冲淡
                    if (c.failed)
                      Text(
                        stripAnsi(c.result!),
                        style: theme.textTheme.bodySmall?.copyWith(
                          fontFamily: 'JetBrains Mono',
                          fontFamilyFallback: CortexTheme.monoFallback,
                          height: 1.5,
                          color: scheme.error,
                        ),
                      )
                    else
                      Text.rich(
                        TextSpan(
                          children: parseAnsi(
                            c.result!,
                            base:
                                (theme.textTheme.bodySmall ?? const TextStyle())
                                    .copyWith(
                                      fontFamily: 'JetBrains Mono',
                                      fontFamilyFallback:
                                          CortexTheme.monoFallback,
                                      height: 1.5,
                                      color: scheme.onSurfaceVariant,
                                    ),
                            // 深色主题要亮档色板 —— 不传的话 cargo 的红字
                            // 在深底上对比度不够，编译报错恰好最读不清
                            brightness: theme.brightness,
                          ),
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
