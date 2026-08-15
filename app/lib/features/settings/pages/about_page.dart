import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/app_config.dart';
import '../../../state/update_controller.dart';

/// 关于这一页：版本与更新。
class AboutPage extends StatelessWidget {
  const AboutPage({super.key});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.symmetric(horizontal: 4),
      children: [
        const AboutTile(),
        const SizedBox(height: 12),
        Text(
          '编译期默认值：USE_MOCK=${AppConfig.defaultUseMock}，'
          'CORTEX_BASE_URL=${AppConfig.defaultBaseUrl}',
          style: theme.textTheme.labelSmall,
        ),
      ],
    );
  }
}

class AboutTile extends ConsumerWidget {
  const AboutTile({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final version = AppConfig.appVersion.trim();
    final update = ref.watch(updateControllerProvider);
    final notifier = ref.read(updateControllerProvider.notifier);

    final String subtitle;
    if (!notifier.enabled) {
      // 说清**为什么**没有更新功能，而不是干脆不显示这一行。
      // 「我的另一台机器上有这个按钮」是个会被问出来的问题
      subtitle = version.isEmpty ? '开发构建，不检查更新' : '这个平台不自我更新（安装包只有 Windows）';
    } else {
      subtitle = switch (update.phase) {
        UpdatePhase.available => '有新版本 ${update.release?.version}',
        UpdatePhase.downloading => '正在下载新版本…',
        UpdatePhase.ready => '已下载，即将安装',
        UpdatePhase.failed => '上次更新失败：${update.error}',
        UpdatePhase.idle => '已是最新',
      };
    }

    return ListTile(
      contentPadding: EdgeInsets.zero,
      title: Text(version.isEmpty ? 'Cortex（开发构建）' : 'Cortex $version'),
      subtitle: Text(subtitle, style: theme.textTheme.bodySmall),
      trailing: notifier.enabled
          ? TextButton(
              onPressed: update.phase == UpdatePhase.available
                  ? notifier.install
                  : update.phase == UpdatePhase.idle
                  ? notifier.check
                  : null,
              child: Text(
                update.phase == UpdatePhase.available ? '立即更新' : '检查更新',
              ),
            )
          : null,
    );
  }
}
