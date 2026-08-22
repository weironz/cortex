/// 左栏最上面那几条 —— 「我要去哪儿」。
///
/// # 为什么要有这一块
///
/// 在它之前左栏只有会话列表，于是「地方」这个概念在界面上根本不存在：
/// 应用等于对话。加画廊时才发现无处安放 —— 挂进设置里，它就成了「配置」
/// （而它是内容）；做成整屏路由，侧栏跟着消失（而用户要的是在两边来回走）。
///
/// ChatGPT 的分法：侧栏顶上一小块导航（新聊天 / 图片 / 资料库…），
/// 下面才是聊天历史。**导航与历史是两类东西**，靠位置分开比靠标题分开省力。
///
/// # 为什么不做一个可注册的表
///
/// 现在三条，而第三条（项目）进来时改这里花了四行 —— 当初不搞注册表
/// 这个决定因此是划算的。同样的话对第四条仍然成立。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../state/app_providers.dart';
import '../../../state/chat_controller.dart';
import '../../../state/project_controller.dart';

class NavBlock extends ConsumerWidget {
  const NavBlock({super.key, this.onNavigated});

  /// 窄屏上左栏是抽屉，选完要把它关掉。
  final VoidCallback? onNavigated;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final view = ref.watch(mainViewProvider);

    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 8, 8, 4),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // 「新建会话」**不是一个地方，是一个动作** —— 所以它不参与
          // 选中态。画成选中会让人以为「我现在在新建会话这一页」
          _NavItem(
            key: const ValueKey('nav:new'),
            icon: Icons.edit_outlined,
            label: '新聊天',
            selected: false,
            onTap: () {
              ref.read(chatControllerProvider.notifier).createSession();
              ref.read(projectControllerProvider.notifier).select(null);
              ref.read(mainViewProvider.notifier).go(MainView.chat);
              onNavigated?.call();
            },
          ),
          const SizedBox(height: 2),
          _NavItem(
            key: const ValueKey('nav:projects'),
            icon: Icons.folder_outlined,
            label: '项目',
            selected: view == MainView.projects,
            onTap: () {
              ref.read(mainViewProvider.notifier).go(MainView.projects);
              onNavigated?.call();
            },
          ),
          const SizedBox(height: 2),
          _NavItem(
            key: const ValueKey('nav:images'),
            icon: Icons.image_outlined,
            label: '图片',
            selected: view == MainView.images,
            onTap: () {
              ref.read(mainViewProvider.notifier).go(MainView.images);
              onNavigated?.call();
            },
          ),
        ],
      ),
    );
  }
}

class _NavItem extends StatefulWidget {
  const _NavItem({
    super.key,
    required this.icon,
    required this.label,
    required this.selected,
    required this.onTap,
  });

  final IconData icon;
  final String label;
  final bool selected;
  final VoidCallback onTap;

  @override
  State<_NavItem> createState() => _NavItemState();
}

class _NavItemState extends State<_NavItem> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final tokens = theme.cortex;

    // 与会话行**同一套**选中/悬停配色（`session_list.dart` 那一处）。
    // 两处不一致的表现是同一栏里两种选中态，用户会以为它们是两种东西
    final Color background;
    if (widget.selected) {
      background = tokens.sidebarAccent;
    } else if (_hovered) {
      background = tokens.sidebarAccent.withValues(alpha: 0.55);
    } else {
      background = Colors.transparent;
    }

    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          decoration: BoxDecoration(
            color: background,
            borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
          ),
          padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
          child: Row(
            children: [
              Icon(
                widget.icon,
                size: 16,
                color: widget.selected
                    ? theme.colorScheme.onSurface
                    : tokens.foregroundTertiary,
              ),
              const SizedBox(width: 10),
              Text(
                widget.label,
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurface,
                  fontWeight: widget.selected ? FontWeight.w600 : null,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
