/// 输入框底部的「云沙箱」开关。**只在 Web 端出现。**
///
/// # 为什么桌面端没有它
///
/// 桌面端的 agent 跑在你自己的机器上（`cortex-local` 直连），文件与命令本来
/// 就有，而且动的是你真正想改的那些文件。给它也放一个「云沙箱」开关，等于
/// 提供一个**更弱**的选项：跑在一个看不见你项目的容器里。
///
/// 所以这不是「Web 端缺一个功能」，是两种部署各自的正确形态 ——
/// 见 `docs/sandbox.md`。
///
/// # 为什么是开关而不是默认开着
///
/// 一个沙箱容器占几百 MB 内存。绝大多数对话不需要文件与命令，
/// 「帮我想一下这段话怎么写」不该顺手拉起一个容器。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/local_agent.dart';
import '../../../state/app_providers.dart';

class SandboxChip extends ConsumerWidget {
  const SandboxChip({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    // 有本地 agent 的构建（桌面端）压根不显示。**判据是能力而不是平台** ——
    // `kIsWeb` 散在界面代码里，是这个仓库刻意避开的形状（见 local_agent.dart）
    if (kLocalAgentSupported) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final on = ref.watch(sandboxProvider);

    return Tooltip(
      message: on
          ? '这一轮在云端沙箱里跑：有文件与命令工具，工作区是容器里的 /workspace。'
                '文件跨会话保留，容器闲置一段时间会自动回收。'
          : '打开后这一轮在云端沙箱里跑 —— agent 能读写文件、执行命令。'
                '关着的时候它只能聊天与检索记忆。',
      child: InkWell(
        borderRadius: BorderRadius.circular(6),
        onTap: () => ref.read(sandboxProvider.notifier).toggle(),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 3),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                on ? Icons.dns_rounded : Icons.dns_outlined,
                size: 14,
                color: on ? scheme.secondary : scheme.onSurfaceVariant,
              ),
              const SizedBox(width: 5),
              Text(
                '云沙箱',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: on ? scheme.secondary : scheme.onSurfaceVariant,
                  fontWeight: on ? FontWeight.w600 : null,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
