import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../models/account.dart';
import '../../../state/app_providers.dart';
import '../../../state/auth_controller.dart';
import '../../settings/settings_sheet.dart';
import 'update_indicator.dart';

/// 左栏最底下那一条：我是谁 + 设置 / 退出 + 更新提示。
///
/// # 为什么在这儿
///
/// 与 Codex / Claude 桌面端一致。更实在的理由是：账号是**整个侧栏的语境**
/// —— 上面那些会话、那个工作区，都属于这个账号。把它放在顶栏会与「设置」
/// 「主题」这类应用级开关混成一排，而它们与「我是谁」无关。
///
/// 左栏收起时它跟着一起消失，这是刻意的（两家参考产品都是这样）：
/// 收起的意思就是整条侧栏都不要了。
class AccountBar extends ConsumerWidget {
  const AccountBar({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final config = ref.watch(appConfigProvider);
    final account = ref.watch(accountProvider).value;
    final health = ref.watch(healthProvider).value;

    final label = accountLabel(
      account: account,
      useMock: config.useMock,
      offline: config.offline,
      authDisabled: health?.authDisabled ?? false,
    );

    return Container(
      decoration: BoxDecoration(
        border: Border(top: BorderSide(color: scheme.outlineVariant)),
      ),
      padding: const EdgeInsets.fromLTRB(8, 6, 6, 6),
      child: Row(
        children: [
          Expanded(
            child: _MenuButton(
              label: label,
              // schema 只在排障时有用，所以放 tooltip 而不是正文：
              // 「我看到的是不是我自己的库」在多租户里是个会被问出口的问题
              tooltip: account == null
                  ? label
                  : '${account.username}\n记忆库：${account.schemaName}',
              avatarText: avatarLetter(label),
            ),
          ),
          const UpdateIndicator(),
        ],
      ),
    );
  }
}

/// 账号栏上显示什么。
///
/// # 没有用户名时必须说清是**哪一种**没有
///
/// 留空或者一律写「未登录」，会把四种完全不同的处境压成一句话，而其中
/// 三种其实一切正常。「我另一台机器上这里有名字」是个会被问出来的问题，
/// 答案得摆在原地。
///
/// 顺序有讲究：mock 与离线是**客户端自己知道**的事实，比服务端答什么更确定，
/// 所以排在前面。
String accountLabel({
  required Account? account,
  required bool useMock,
  required bool offline,
  required bool authDisabled,
}) {
  if (useMock) return 'Mock 数据源';
  if (offline) return '离线 · 无记忆';
  if (account != null && account.username.trim().isNotEmpty) {
    return account.username;
  }
  if (authDisabled) return '未启用认证';
  // 老服务端没有 /auth/me，或者这次没查到。连是连上了，只是没有名字 ——
  // 不能因此把这一条变成报错，它还挂着设置与退出登录
  return '已连接';
}

/// 头像里那个字。
///
/// 取第一个字符而不是首字母缩写：中文用户名没有「首字母」，
/// 而 `willai` 与 `王` 都该给出一个能看的圆。
String avatarLetter(String label) {
  final t = label.trim();
  return t.isEmpty ? '?' : t.characters.first.toUpperCase();
}

class _MenuButton extends ConsumerWidget {
  const _MenuButton({
    required this.label,
    required this.tooltip,
    required this.avatarText,
  });

  final String label;
  final String tooltip;
  final String avatarText;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return PopupMenuButton<String>(
      tooltip: tooltip,
      position: PopupMenuPosition.over,
      onSelected: (value) {
        switch (value) {
          case 'settings':
            unawaited(showSettingsSheet(context));
          case 'signout':
            unawaited(ref.read(authControllerProvider.notifier).signOut());
        }
      },
      itemBuilder: (context) => [
        const PopupMenuItem(
          value: 'settings',
          child: ListTile(
            dense: true,
            contentPadding: EdgeInsets.zero,
            leading: Icon(Icons.tune_rounded, size: 18),
            title: Text('设置'),
          ),
        ),
        const PopupMenuItem(
          value: 'signout',
          child: ListTile(
            dense: true,
            contentPadding: EdgeInsets.zero,
            leading: Icon(Icons.logout_rounded, size: 18),
            title: Text('退出登录'),
          ),
        ),
      ],
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 4, horizontal: 2),
        child: Row(
          children: [
            CircleAvatar(
              radius: 13,
              backgroundColor: scheme.primaryContainer,
              child: Text(
                avatarText,
                style: theme.textTheme.labelMedium?.copyWith(
                  color: scheme.onPrimaryContainer,
                ),
              ),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                label,
                style: theme.textTheme.bodyMedium,
                overflow: TextOverflow.ellipsis,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
