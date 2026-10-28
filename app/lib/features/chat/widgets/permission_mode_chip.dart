/// 输入框底部那个权限档位开关。
///
/// 形态照 Claude Code：一个 chip 显示当前档，点开弹三档。
///
/// # 为什么「完全放行」要单独确认一次
///
/// 另外两档改变的是**打扰频率**，选错了下一次弹窗就知道了。这一档改变的是
/// **有没有人在把关**：在没有内核沙箱的机器上（Windows 就是），
/// agent 的每条命令都是裸跑的，而这件事在界面上没有任何别的症状。
///
/// 确认之后 chip 一直是警示色 —— 不让它看起来像个已经忘掉的普通设置。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/permission_mode.dart';
import '../../../state/app_providers.dart';
import '../../../core/theme.dart';
import '../../../widgets/permission_confirmation.dart';

class PermissionModeChip extends ConsumerWidget {
  const PermissionModeChip({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final mode = ref.watch(permissionModeProvider);
    final danger = mode == PermissionMode.bypass;

    return Tooltip(
      message: mode.blurb,
      child: InkWell(
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        onTap: () => _open(context, ref, mode),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 3),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                danger ? Icons.lock_open_rounded : Icons.shield_outlined,
                size: 13,
                color: danger ? scheme.error : scheme.onSurfaceVariant,
              ),
              const SizedBox(width: 4),
              Text(
                mode.label,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: danger ? scheme.error : scheme.onSurfaceVariant,
                  fontWeight: danger ? FontWeight.w700 : null,
                ),
              ),
              Icon(
                Icons.arrow_drop_up_rounded,
                size: 15,
                color: danger ? scheme.error : scheme.onSurfaceVariant,
              ),
            ],
          ),
        ),
      ),
    );
  }

  Future<void> _open(
    BuildContext context,
    WidgetRef ref,
    PermissionMode current,
  ) async {
    final picked = await showModalBottomSheet<PermissionMode>(
      context: context,
      showDragHandle: true,
      builder: (ctx) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            for (final m in PermissionMode.values)
              ListTile(
                leading: Icon(
                  m == PermissionMode.bypass
                      ? Icons.lock_open_rounded
                      : Icons.shield_outlined,
                  color: m == PermissionMode.bypass
                      ? Theme.of(ctx).colorScheme.error
                      : null,
                ),
                title: Text(m.label),
                subtitle: Text(m.blurb),
                trailing: m == current ? const Icon(Icons.check_rounded) : null,
                onTap: () => Navigator.of(ctx).pop(m),
              ),
          ],
        ),
      ),
    );
    if (picked == null || picked == current) return;
    if (!context.mounted) return;

    if (!await confirmPermissionChange(context, current, picked)) {
      return;
    }
    if (!context.mounted) return;
    ref.read(permissionModeProvider.notifier).set(picked);
  }
}
