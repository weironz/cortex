import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/app_config.dart';
import '../../../core/link_launcher.dart';
import '../../../state/update_controller.dart';

const String _changelogUrl =
    'https://github.com/weironz/cortex/blob/main/CHANGELOG.md';

/// 常驻的「关于 / 更新」小图标。
///
/// # 平时是关于，有更新时是更新
///
/// 一个一直在的图标，多数时候点开是「关于」（版本号、更新日志）——
/// 这个入口本来就一直缺着。有新版本时它右上角多一个点，**点一下就把更新
/// 走完**：下载、校验、装、重启。
///
/// 之所以不是弹窗：roadmap 里那条「刻意没有开工」写着，做一半的更新器
/// 「每次开机提示一次，比没有更糟」。一个安静的点谁都不打扰，而想升级的人
/// 一眼就能看见。
///
/// # 为什么在这一行，而不是左下角
///
/// 参照的 ChatGPT 把它放在左下角账号栏旁边，而**我们现在没有账号栏**
/// （退出登录在设置页里）。为一个图标先造一整条账号栏是另一件事。
/// 顶栏这一行就是本应用等价的常驻小图标区：`SyncIndicator`、后端徽标、
/// 设置、主题都在这儿。
class UpdateIndicator extends ConsumerWidget {
  const UpdateIndicator({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final state = ref.watch(updateControllerProvider);
    final scheme = Theme.of(context).colorScheme;

    final Widget icon;
    final VoidCallback? onTap;
    switch (state.phase) {
      case UpdatePhase.downloading:
        icon = SizedBox(
          width: 15,
          height: 15,
          child: CircularProgressIndicator(
            strokeWidth: 2,
            // 服务端没给长度时 value 是 null —— 转不定进度的圈。
            // 编一个假进度比不知道更糟
            value: state.progress,
            color: scheme.primary,
          ),
        );
        onTap = null;
      case UpdatePhase.ready:
        icon = SizedBox(
          width: 15,
          height: 15,
          child: CircularProgressIndicator(
            strokeWidth: 2,
            color: scheme.primary,
          ),
        );
        onTap = null;
      case UpdatePhase.failed:
        icon = Icon(Icons.error_outline_rounded, size: 18, color: scheme.error);
        onTap = ref.read(updateControllerProvider.notifier).retry;
      case UpdatePhase.available:
        icon = _Dotted(
          color: scheme.primary,
          child: const Icon(Icons.help_outline_rounded, size: 18),
        );
        onTap = ref.read(updateControllerProvider.notifier).install;
      case UpdatePhase.idle:
        icon = const Icon(Icons.help_outline_rounded, size: 18);
        onTap = () => showAboutCortex(context);
    }

    return Tooltip(
      message: _tooltip(state),
      child: IconButton(onPressed: onTap, iconSize: 18, icon: icon),
    );
  }

  static String _tooltip(UpdateState s) => switch (s.phase) {
    UpdatePhase.idle => '关于 Cortex',
    UpdatePhase.available =>
      '有新版本 ${s.release?.version ?? ''} —— 点一下自动下载、安装并重启',
    UpdatePhase.downloading =>
      s.progress == null
          ? '正在下载 ${s.release?.version ?? ''}…'
          : '正在下载 ${s.release?.version ?? ''} ${(s.progress! * 100).round()}%',
    UpdatePhase.ready =>
      s.waitingForTurn ? '已下载并校验通过 —— 这轮回答结束后自动安装' : '正在安装，马上重启…',
    UpdatePhase.failed => '更新失败：${s.error ?? '原因不明'}\n点击重试',
  };
}

/// 图标右上角那个点。
class _Dotted extends StatelessWidget {
  const _Dotted({required this.child, required this.color});

  final Widget child;
  final Color color;

  @override
  Widget build(BuildContext context) => Stack(
    clipBehavior: Clip.none,
    children: [
      child,
      Positioned(
        right: -1,
        top: -1,
        child: Container(
          width: 7,
          height: 7,
          decoration: BoxDecoration(color: color, shape: BoxShape.circle),
        ),
      ),
    ],
  );
}

/// 「关于」面板：这份构建是哪一版、改了什么、手动查一次更新。
Future<void> showAboutCortex(BuildContext context) async {
  final ref = ProviderScope.containerOf(context);
  await showDialog<void>(
    context: context,
    builder: (ctx) {
      final version = AppConfig.appVersion.trim();
      return AlertDialog(
        title: const Text('关于 Cortex'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              // 空版本号说明这不是发布构建。明写出来，而不是显示一个
              // 看着像真的的 0.0.0
              version.isEmpty ? '开发构建（没有版本号）' : '版本 $version',
              style: Theme.of(ctx).textTheme.bodyMedium,
            ),
            const SizedBox(height: 12),
            Text('记忆原生的通用 AI Agent', style: Theme.of(ctx).textTheme.bodySmall),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => openExternalLink(ctx, _changelogUrl),
            child: const Text('更新日志'),
          ),
          if (ref.read(updateControllerProvider.notifier).enabled)
            TextButton(
              onPressed: () {
                Navigator.of(ctx).pop();
                ref.read(updateControllerProvider.notifier).check();
              },
              child: const Text('检查更新'),
            ),
          FilledButton(
            onPressed: () => Navigator.of(ctx).pop(),
            child: const Text('好'),
          ),
        ],
      );
    },
  );
}
