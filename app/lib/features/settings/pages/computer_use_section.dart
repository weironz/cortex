/// 「操作这台电脑」那一节 —— 内容本体，壳在 [`computer_use_page.dart`]。
///
/// # 为什么从连接器页搬走了
///
/// 原先它压在连接器页最下面，理由是「回答的是同一个问题：模型手上有什么」。
/// 那个归类没错，但它把**权限最大的一组能力**藏在了一页的末尾 ——
/// 要往下滚过一整列 MCP server 才看得见。别的都是「接一台别人写的 server」，
/// 只有这一条交出去的是你这台机器的屏幕与键鼠；它该有自己的一页。
///
/// 内容留在这个文件里没动，页面只是个壳 —— 这样搬家不碰那段措辞。
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

/// 这个部署做不做得到「操作电脑」。
///
/// # ⚠️ 抽成 provider 是为了**只有一个判据**
///
/// 设置目录里的那一项与这一页的内容各判各的话，会长出两种都很难看的状态：
/// 菜单里有这一项、点进去一片空白；或者反过来，能力在但没有入口。
/// 这正是这个仓库的头号故障形状（造好了没人调用）的近亲 —— 判据分叉。
///
/// 所以它有且只有这一处定义，[`ComputerUseSection`] 与
/// `settings_sheet.dart` 的菜单闸都读它。
final computerUseAvailableProvider = Provider<bool>(
  (ref) => ref.watch(healthProvider).value?.computerUse == true,
);

class ComputerUseSection extends ConsumerWidget {
  const ComputerUseSection({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);

    // 做不到就整节不画。**不是画一个灰的开关** —— 灰开关读起来像
    // 「你可以去别处打开它」，而这里根本没有那个别处
    if (!ref.watch(computerUseAvailableProvider)) {
      return const SizedBox.shrink();
    }

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
