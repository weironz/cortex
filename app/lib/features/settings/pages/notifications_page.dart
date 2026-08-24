/// 「通知」这一页。
///
/// # 它现在只有声音，而这一页要**说清楚**
///
/// 系统级通知（窗口最小化时弹一个横幅）还没有 —— 那要一个通知插件，
/// 会改动三个平台 runner 的构建。roadmap G 节里记着这件事，而这一页
/// 的底注就是它在界面上的样子：**宁可少一个开关，不要一个打开也不
/// 发生任何事的开关**（CLAUDE.md 约束 2）。
///
/// 不说的话，用户会因为「我明明开了通知」而在最小化之后错过一次确认，
/// 然后认定通知功能坏了 —— 而它从来就没有那一半。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../state/notify_prefs.dart';
import '../widgets/settings_layout.dart';

class NotificationsPage extends ConsumerWidget {
  const NotificationsPage({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final prefs = ref.watch(notifyPrefsProvider);
    final n = ref.read(notifyPrefsProvider.notifier);

    return ListView(
      padding: EdgeInsets.zero,
      children: [
        SettingsSection(
          title: '提示音',
          description: '窗口在后台时，一声提示足够把你叫回来。',
          children: [
            SettingsCard(
              child: Column(
                children: [
                  SwitchListTile(
                    key: const ValueKey('notify:confirm'),
                    contentPadding: EdgeInsets.zero,
                    value: prefs.onConfirm,
                    onChanged: n.setOnConfirm,
                    title: const Text('有一轮在等你确认'),
                    subtitle: Text(
                      // 说清它比另一档更该开：这是唯一「不管就永远卡住」的
                      '这是唯一不处理就会一直卡着的状态。',
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: theme.cortex.foregroundTertiary,
                      ),
                    ),
                  ),
                  SwitchListTile(
                    key: const ValueKey('notify:finish'),
                    contentPadding: EdgeInsets.zero,
                    value: prefs.onFinish,
                    onChanged: n.setOnFinish,
                    title: const Text('一轮跑完了'),
                    subtitle: Text(
                      // 说清判据，否则「我盯着看它跑完怎么不响」会被当成 bug
                      '只在你人在别处时响 —— 盯着看它跑完的话不打扰你。',
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: theme.cortex.foregroundTertiary,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
        SettingsSection(
          description: '窗口最小化之后就看不到应用内的提示了。系统级通知'
              '（桌面右下角那种横幅）还没有做 —— 在它落地之前，这里不'
              '声称「会通知你」，只声称「回来就看得见」。',
          children: const [],
        ),
      ],
    );
  }
}
