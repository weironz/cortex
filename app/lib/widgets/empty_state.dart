import 'package:flutter/material.dart';

import '../core/theme.dart';

class EmptyState extends StatelessWidget {
  const EmptyState({
    super.key,
    required this.icon,
    required this.title,
    this.description,
    this.action,
    this.tone = EmptyStateTone.neutral,
  });

  final IconData icon;
  final String title;
  final String? description;
  final Widget? action;
  final EmptyStateTone tone;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final accent = switch (tone) {
      EmptyStateTone.neutral => scheme.onSurfaceVariant,
      EmptyStateTone.error => scheme.error,
    };

    // Center 在**无界**约束（ListView / Column 里）会收缩、在**有界**约束
    // （Expanded 给的）会居中撑满 —— 两种宿主都存在。内容比宿主矮时两种
    // 都对；**内容更高时**（界面缩放 175% 的长错误串就是必现场景），
    // 有界宿主里它会竖直溢出报黄黑纹。
    //
    // 所以外面套一层 SingleChildScrollView：装得下时它什么都不做，
    // 装不下时可以滚 —— 两种宿主、任何缩放档都不炸。
    return SingleChildScrollView(
      child: Center(
        child: Padding(
          padding: const EdgeInsets.all(28),
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 380),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Container(
                  width: 46,
                  height: 46,
                  decoration: BoxDecoration(
                    color: accent.withValues(alpha: 0.10),
                    borderRadius: BorderRadius.circular(CortexTokens.radiusXl),
                  ),
                  child: Icon(icon, size: 22, color: accent),
                ),
                const SizedBox(height: 14),
                Text(
                  title,
                  textAlign: TextAlign.center,
                  style: theme.textTheme.titleMedium,
                ),
                if (description != null) ...[
                  const SizedBox(height: 6),
                  Text(
                    description!,
                    textAlign: TextAlign.center,
                    style: theme.textTheme.bodySmall,
                  ),
                ],
                if (action != null) ...[const SizedBox(height: 18), action!],
              ],
            ),
          ),
        ),
      ),
    );
  }
}

enum EmptyStateTone { neutral, error }
