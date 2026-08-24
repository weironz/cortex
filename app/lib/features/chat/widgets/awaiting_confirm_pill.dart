/// 顶栏那颗「等你确认 · N」——第四状态在应用级的落点。
///
/// # 为什么顶栏要有它
///
/// 会话行上的琥珀点只有**翻到那一行**才看得见：等确认的会话在折叠的分组里、
/// 在滚动区外、甚至在「项目」段收起时，人完全不知道有东西卡着。而这个状态
/// 是唯一「不处理就永远卡住」的 —— 超时后服务端按拒绝处理，一轮就白跑了。
///
/// 所以它要有一个**任何滚动位置都可见**的落点。N > 0 才画；点一下直达
/// 第一条在等的会话。颜色必须与会话行的点同一个 token（`tokens.warning`），
/// 各配各的迟早漂成两种黄。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../state/app_providers.dart';
import '../../../state/chat_controller.dart';
import '../../../state/confirm_controller.dart';

class AwaitingConfirmPill extends ConsumerWidget {
  const AwaitingConfirmPill({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final awaiting = ref.watch(awaitingConfirmSessionsProvider);
    if (awaiting.isEmpty) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final tokens = theme.cortex;
    // 当前就停在那条会话上时不画：确认面板本身就在眼前，
    // 一颗指向「你正看着的东西」的 pill 是噪音
    final active = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    final elsewhere = awaiting.where((id) => id != active).toList()..sort();
    if (elsewhere.isEmpty) return const SizedBox.shrink();

    return Padding(
      padding: const EdgeInsets.only(right: 4),
      child: Tooltip(
        message: '${elsewhere.length} 条会话在等你确认才能继续，点击前往',
        child: Material(
          color: tokens.warning.withValues(alpha: 0.12),
          borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
          child: InkWell(
            borderRadius: BorderRadius.circular(CortexTokens.radiusPill),
            onTap: () {
              ref
                  .read(chatControllerProvider.notifier)
                  .selectSession(elsewhere.first);
              ref.read(mainViewProvider.notifier).go(MainView.chat);
            },
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Container(
                    width: 7,
                    height: 7,
                    decoration: BoxDecoration(
                      shape: BoxShape.circle,
                      border: Border.all(color: tokens.warning, width: 2),
                    ),
                  ),
                  const SizedBox(width: 6),
                  Text(
                    '等你确认 · ${elsewhere.length}',
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: tokens.warning,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
