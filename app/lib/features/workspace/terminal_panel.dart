/// 终端面板 —— 标签栏（新建 / 关掉这一个）+ 铺满 + 收起，下面是 shell。
///
/// # 为什么它从右栏的页签里搬了出来
///
/// 它从前与「文件 / 本轮改动」并排当第三个页签。那套心智
/// （「打开文件栏，然后在里面翻」）对**看**的东西成立，对终端不成立：
/// 终端是**开着干活**的地方，它要自己的标签页、自己的开关、自己的快捷键，
/// 还要能铺满整块内容区。塞在别人的页签条里，这四样一样都摆不下。
///
/// 它仍然住在同一列（右侧只有一列，见 `AppShell`）—— 与文件栏轮流占，
/// 而不是再开第四列把对话挤掉。
///
/// # ⚠️ 切标签**不能**销毁没在前面的那几个
///
/// 每个标签背后是一个真的 shell 子进程，部件一销毁 WS 就关、agent 那侧
/// 就收掉它。所以这里用 `IndexedStack` 而不是「只画当前那个」：前者把
/// 所有孩子都留在树上（也都参与布局，于是它们的 resize 消息是对的），
/// 只画选中的那一个。
///
/// 换成 `if (i == active)` 的写法，症状是「切到终端 2 再切回来，
/// 终端 1 里跑着的东西没了」——而且没有任何报错。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/ansi.dart';
import '../../core/terminal_channel.dart';
import '../../core/theme.dart';
import '../../models/tool_call.dart';
import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../../state/terminal_panel.dart';
import 'interactive_terminal.dart';

class TerminalPanel extends ConsumerWidget {
  const TerminalPanel({super.key, this.canExpand = true});

  /// 画不画那个「铺满」按钮。
  ///
  /// 窄屏上终端是从底部升起的那张纸，它已经占了 86% 的高度 ——
  /// 再给一个「铺满」按钮，点下去看不出区别。约束 2：做不到的不摆出来。
  final bool canExpand;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final st = ref.watch(terminalPanelProvider);

    final origin = ref.watch(localAgentOriginProvider).value;
    final sessionId = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    // 真终端要三样都在：这个构建连得了（Web 恒 false）、本机 agent 活着、
    // 有选中的会话（shell 的 cwd 跟着会话的工作区绑定走）。
    // 缺任何一样退回只读的命令记录 —— 那在所有构建上都是真的。
    // ⚠️ 判据只此一处：摆一个连不上的终端就是约束 2 说的那种谎
    final live =
        kInteractiveTerminalSupported && origin != null && sessionId != null;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _TabBar(live: live, canExpand: canExpand),
        Divider(height: 1, color: theme.cortex.sidebarBorder),
        Expanded(
          child: live
              ? IndexedStack(
                  index: st.activeIndex,
                  children: [
                    for (final t in st.tabs)
                      InteractiveTerminal(
                        // key 用标签 id + 会话：换会话时簿子会清空重来，
                        // 而 id 是新的，于是旧 shell 随部件一起销毁
                        key: ValueKey('term:${t.id}'),
                        origin: origin,
                        sessionId: sessionId,
                        // read 而不是 watch：这把凭据钉死在 agent 启动那一刻，
                        // agent 换代时 origin 必然跟着变，上面那个 watch
                        // 已经会重建这里
                        token: ref
                            .read(localAgentHandleProvider)
                            .pinnedCredential,
                      ),
                  ],
                )
              : const _CommandLog(),
        ),
      ],
    );
  }
}

/// 标签 + 加号 + 铺满 + 收起。
class _TabBar extends ConsumerWidget {
  const _TabBar({required this.live, required this.canExpand});

  final bool live;
  final bool canExpand;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final st = ref.watch(terminalPanelProvider);
    final panel = ref.read(terminalPanelProvider.notifier);

    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 6, 6, 4),
      child: Row(
        children: [
          Expanded(
            child: live
                // 标签多了要能横着划开，而不是把加号挤出可视区
                ? SingleChildScrollView(
                    scrollDirection: Axis.horizontal,
                    child: Row(
                      children: [
                        for (final t in st.tabs) ...[
                          _Tab(
                            label: t.label,
                            selected: t.id == st.activeId,
                            onTap: () => panel.select(t.id),
                            onClose: () => panel.close(t.id),
                          ),
                          const SizedBox(width: 4),
                        ],
                        IconButton(
                          onPressed: panel.addTab,
                          iconSize: 16,
                          visualDensity: VisualDensity.compact,
                          tooltip: '新建终端',
                          icon: const Icon(Icons.add_rounded),
                        ),
                      ],
                    ),
                  )
                : Text(
                    // 只读那一支没有标签可切 —— 给它一个说得出自己是什么
                    // 的标题，而不是一条空白的标签栏
                    '本轮跑过的命令',
                    style: theme.textTheme.labelMedium?.copyWith(
                      color: theme.cortex.foregroundTertiary,
                    ),
                  ),
          ),
          if (canExpand)
            IconButton(
              key: const ValueKey('terminal:expand'),
              onPressed: panel.toggleExpanded,
              iconSize: 17,
              visualDensity: VisualDensity.compact,
              tooltip: st.expanded ? '收回右侧一列' : '铺满',
              icon: Icon(
                st.expanded
                    ? Icons.close_fullscreen_rounded
                    : Icons.open_in_full_rounded,
              ),
            ),
          IconButton(
            key: const ValueKey('terminal:hide'),
            onPressed: panel.hide,
            iconSize: 17,
            visualDensity: VisualDensity.compact,
            // ⚠️ 说「收起」不说「关闭」：它不动任何一个 shell。
            // 标签上那个 × 才是关 shell 的，两处措辞必须分得开 ——
            // 否则用户会以为点了这个就把 `npm run dev` 停了（或者反过来，
            // 以为没停）
            tooltip: '收起终端（不关掉里面跑着的东西）',
            icon: const Icon(Icons.close_rounded),
          ),
        ],
      ),
    );
  }
}

class _Tab extends StatelessWidget {
  const _Tab({
    required this.label,
    required this.selected,
    required this.onTap,
    required this.onClose,
  });

  final String label;
  final bool selected;
  final VoidCallback onTap;
  final VoidCallback onClose;

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
          padding: const EdgeInsets.fromLTRB(10, 5, 4, 5),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                label,
                style: theme.textTheme.labelMedium?.copyWith(
                  fontWeight: selected ? FontWeight.w600 : null,
                  color: selected
                      ? theme.colorScheme.onSurface
                      : tokens.foregroundTertiary,
                ),
              ),
              const SizedBox(width: 4),
              // 每个标签都带 ×（不是只有选中那个带）：只给选中的话，
              // 关掉一个非当前标签要先点两次 —— 而「切过去」这一步会把
              // 那个 shell 的输出滚到眼前，正是不想看才要关它
              InkWell(
                onTap: onClose,
                borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
                child: Padding(
                  padding: const EdgeInsets.all(2),
                  child: Icon(
                    Icons.close_rounded,
                    size: 13,
                    color: tokens.foregroundTertiary,
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 这一轮跑过的 shell 命令与输出，按时间顺序 —— 没有本地 shell 时的形态。
///
/// Web 端与「agent 没起来」都落到这里。它不是降级的终端，是**另一件事**：
/// 模型跑过什么，看得见。
class _CommandLog extends ConsumerWidget {
  const _CommandLog();

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
      itemBuilder: (context, i) => _LoggedCall(call: calls[i]),
    );
  }
}

/// 只读命令记录里的一条。
class _LoggedCall extends StatelessWidget {
  const _LoggedCall({required this.call});

  final ToolCall call;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final c = call;
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
                        base: (theme.textTheme.bodySmall ?? const TextStyle())
                            .copyWith(
                              fontFamily: 'JetBrains Mono',
                              fontFamilyFallback: CortexTheme.monoFallback,
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
  }
}
