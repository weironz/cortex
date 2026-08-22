/// 「操作这台电脑」那一节 —— 连接器页最下面的开关。
///
/// # 为什么在连接器页，而不是自己一页
///
/// 它回答的是同一个问题：**模型手上有什么**。MCP 给的是别人写的工具，
/// 这个给的是这台机器的屏幕与键鼠 —— 用户在一个地方看完「它能干什么」，
/// 比在设置目录里多一项好。
///
/// # ⚠️ 做不到的时候**整节不画**
///
/// 做不到有两种情形：跑在容器里（云端会话，没有屏幕），或者这个构建没编进
/// 那一组（Linux 桌面）。两种都不是「关着」而是「没有」——
/// 摆一个打开也没用的开关，比没有这个开关更糟（CLAUDE.md 约束 2）。
/// 判据来自 `/health` 的 `computer_use`，是 agent 自己报的。
///
/// # 开关旁边那段话不是免责声明
///
/// 用户要做的是一个**知情**的决定，而做这个决定需要知道两件他不会自己想到
/// 的事：截图里有屏幕上的一切（包括别的窗口），以及点击落在真实的按钮上。
/// 所以那段话写的是「它能看见什么、能做什么」，不是「风险自负」。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../state/app_providers.dart';
import '../../../state/model_controller.dart';
import '../widgets/settings_layout.dart';

class ComputerUseSection extends ConsumerWidget {
  const ComputerUseSection({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final health = ref.watch(healthProvider).value;

    // 做不到就整节不画。**不是画一个灰的开关** —— 灰开关读起来像
    // 「你可以去别处打开它」，而这里根本没有那个别处
    if (health?.computerUse != true) return const SizedBox.shrink();

    final on = ref.watch(computerUseProvider);
    // 模型看不懂图 = 这个开关打开也不生效。**必须当场说** ——
    // 不说的话，用户打开它、问一句「看看我屏幕」，得到的是一条英文 400，
    // 而**屏幕已经拍过、也已经发出去了**（截图在工具执行时就拍了）
    final blind = ref.watch(selectedModelVisionProvider) == false;

    return SettingsSection(
      title: '操作这台电脑',
      description: '让模型看见你的屏幕，并且能动鼠标键盘。',
      children: [
        SettingsCard(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text('允许操作电脑', style: theme.textTheme.bodyMedium),
                        const SizedBox(height: 3),
                        Text(
                          switch ((on, blind)) {
                            (true, true) => '开着，但当前这个模型看不懂图 —— 这一轮不会带上这组工具。',
                            (true, false) => '开着。每一次操作仍然按你选的权限档来问你。',
                            _ => '关着。模型看不见屏幕，也动不了鼠标键盘。',
                          },
                          style: theme.textTheme.labelSmall?.copyWith(
                            color: on && blind
                                ? theme.colorScheme.error
                                : theme.cortex.foregroundTertiary,
                          ),
                        ),
                      ],
                    ),
                  ),
                  Switch(
                    key: const ValueKey('computer_use:toggle'),
                    value: on,
                    onChanged: (v) =>
                        ref.read(computerUseProvider.notifier).set(on: v),
                  ),
                ],
              ),
              if (on && blind) ...[
                const SizedBox(height: 10),
                _Point(
                  icon: Icons.visibility_off_outlined,
                  text:
                      '当前模型不支持看图，所以这一组工具不会摆给它 —— '
                      '摆了的话它会先把你的屏幕拍下来发出去，再收到一条看不懂的报错。'
                      '换一个「看得懂图」的模型再用。',
                ),
              ],
              const SizedBox(height: 12),
              // ⚠️ 这两句是这个开关的全部前提。它们讲的是用户**不会自己想到**
              // 的两件事，而不是免责声明
              _Point(
                icon: Icons.visibility_outlined,
                text:
                    '截图里有屏幕上的一切 —— 包括另一个窗口里的聊天、'
                    '打开着的邮箱、密码管理器。图会发给模型，发出去就收不回来。',
              ),
              const SizedBox(height: 8),
              _Point(
                icon: Icons.mouse_outlined,
                text:
                    '点击和输入落在真实的窗口上，不在沙箱里 —— '
                    '文件工具那道工作区围栏管不到这里。',
              ),
              const SizedBox(height: 8),
              _Point(
                icon: Icons.shield_outlined,
                text:
                    '密码、付款、发送这类事它会停下来交回给你，'
                    '这条写在它的提示词里。但别只靠这一条 —— '
                    '权限档留在「每次询问」，你就能逐次看见它要点哪儿。',
              ),
            ],
          ),
        ),
      ],
    );
  }
}

class _Point extends StatelessWidget {
  const _Point({required this.icon, required this.text});

  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Icon(icon, size: 14, color: theme.cortex.foregroundTertiary),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            text,
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.cortex.foregroundTertiary,
              height: 1.5,
            ),
          ),
        ),
      ],
    );
  }
}
