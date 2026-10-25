import 'package:flutter/material.dart';

import '../design_system/design_system.dart';

/// Consistent 44px header for each of the three panes.
///
/// # 为什么高度是 44 而不是 M3 的 56
///
/// 桌面端顶栏是**一条信息带**，不是一屏的主角。56 是从移动端
/// `AppBar` 继承来的（那里要留出状态栏与拇指命中区），在桌面上它
/// 白吃掉一行对话的高度。44 仍然放得下 13.5px 的标题与 11px 的副标题。
class PanelHeader extends StatelessWidget {
  const PanelHeader({
    super.key,
    required this.title,
    this.leading,
    this.actions = const [],
    this.subtitle,
  });

  final String title;
  final Widget? leading;
  final String? subtitle;
  final List<Widget> actions;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Container(
      // ⚠️ **minHeight，不是 height。** 界面缩放（设置 → 外观 → 字体与缩放）
      // 会让标题与副标题的字号一起长，44px 的固定高度在 150% 档就会竖直
      // 溢出 —— 那条黄黑纹出现在每个面板的头顶上。让它能长，
      // 默认档的高度不变（文字 13.5px 时正好也是 44）。
      constraints: const BoxConstraints(minHeight: 44),
      padding: const EdgeInsets.only(
        left: CortexSpace.lg,
        right: CortexSpace.md,
      ),
      decoration: BoxDecoration(
        border: CortexStroke.bottom(scheme.outlineVariant),
      ),
      child: Row(
        children: [
          if (leading != null) ...[
            leading!,
            const SizedBox(width: CortexSpace.sm),
          ],
          Expanded(
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  title,
                  style: theme.textTheme.titleMedium?.copyWith(
                    fontSize: 13.5,
                    fontWeight: FontWeight.w600,
                    letterSpacing: -0.1,
                  ),
                  overflow: TextOverflow.ellipsis,
                ),
                if (subtitle != null)
                  Text(
                    subtitle!,
                    style: theme.textTheme.labelSmall,
                    overflow: TextOverflow.ellipsis,
                  ),
              ],
            ),
          ),
          ...actions,
        ],
      ),
    );
  }
}

/// Uppercase micro-label used to separate groups inside a pane.
///
/// # 为什么是 11px / 0.8 字距 / 第三级前景
///
/// 它是**结构标记**，不是内容：一列扫下来，眼睛应该先读到会话标题，
/// 再在需要定位时看到它。所以它比正文小两档、比正文淡两档，
/// 靠字距（而不是字号）撑出「这是一个标签」的感觉 —— 中文没有大小写，
/// 大写那一招在这里不成立。
class SectionLabel extends StatelessWidget {
  const SectionLabel(this.text, {super.key, this.trailing});

  final String text;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      // 上 14 下 2：分组头属于**它下面那一组**，不是上下均分的分隔物。
      // 均分的话它会读成「两组之间的一条线」
      padding: const EdgeInsets.fromLTRB(
        CortexSpace.lg,
        CortexSpace.lg,
        CortexSpace.lg,
        CortexSpace.xxs,
      ),
      child: Row(
        children: [
          Text(
            text,
            style: theme.textTheme.labelSmall?.copyWith(
              fontWeight: FontWeight.w600,
              letterSpacing: 0.8,
              color: theme.cortex.foregroundTertiary,
            ),
          ),
          const Spacer(),
          ?trailing,
        ],
      ),
    );
  }
}
