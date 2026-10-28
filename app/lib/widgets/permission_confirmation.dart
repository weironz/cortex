import 'package:flutter/material.dart';

import '../core/permission_mode.dart';

/// 输入框和设置页共用确认入口，不能有一处绕过完整访问的告知。
Future<bool> confirmPermissionChange(
  BuildContext context,
  PermissionMode current,
  PermissionMode next,
) async {
  if (next != PermissionMode.bypass || current == next) return true;
  final scheme = Theme.of(context).colorScheme;
  return await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          icon: Icon(Icons.lock_open_rounded, color: scheme.error),
          title: const Text('开启完全放行？'),
          content: const Text(
            '在本机，Cortex 将按当前用户权限直接读写文件、执行命令和打开应用，'
            '不再使用工作区路径围栏或 OS 进程沙箱，也不再弹出操作确认。\n\n'
            '这不会获得管理员权限。云端会话仍限制在容器内，不能操作你的本机桌面。',
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.of(context).pop(false),
              child: const Text('算了'),
            ),
            FilledButton(
              onPressed: () => Navigator.of(context).pop(true),
              style: FilledButton.styleFrom(backgroundColor: scheme.error),
              child: const Text('我明白，开启'),
            ),
          ],
        ),
      ) ??
      false;
}
