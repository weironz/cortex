/// 「权限与沙箱」这一页 —— **这台机器上，agent 能做到什么程度**。
///
/// # 为什么值得一页，而不是把权限档留在输入框那颗 chip 上
///
/// 那颗 chip 管的是**这一轮**（用户在对话里临时放宽一次）。而「我默认
/// 希望它问我」是一个账号级的态度，此前**没有任何地方能表达** ——
/// 每开一条新会话都回到默认档，想要更严或更松的人只能每轮手动改。
///
/// 顺带它也是「这台机器上到底有没有沙箱」的唯一观测点。此前这句话只在
/// `/health` 的 JSON 里（`sandbox` 字段），而那不是用户看得到的地方 ——
/// Windows 上「没有内核沙箱、改由逐条确认把关」是一个**用户必须知道**
/// 的事实：它决定了那些确认框不是走过场。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/permission_mode.dart';
import '../../../core/theme.dart';
import '../../../state/app_providers.dart';
import '../widgets/settings_layout.dart';

class PermissionsPage extends ConsumerWidget {
  const PermissionsPage({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final health = ref.watch(healthProvider).value;
    final mode = ref.watch(permissionModeProvider);

    return ListView(
      padding: EdgeInsets.zero,
      children: [
        SettingsSection(
          title: '默认权限档',
          description: '新会话从哪一档开始。对话里那颗 chip 仍然能临时改这一轮 —— '
              '改了不影响这里，下一条新会话照旧从这一档起步。',
          children: [
            SettingsChoice<PermissionMode>(
              value: mode,
              onChanged: ref.read(permissionModeProvider.notifier).set,
              options: [
                for (final m in PermissionMode.values)
                  (value: m, label: m.label, hint: m.blurb),
              ],
            ),
          ],
        ),
        SettingsSection(
          title: '这台机器上的沙箱',
          description: '工具执行受什么保护 —— 由 agent 进程实测得出，不是配置项。',
          children: [
            SettingsCard(
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Icon(
                    _sandboxOk(health?.sandbox)
                        ? Icons.shield_outlined
                        : Icons.gpp_maybe_outlined,
                    size: 18,
                    // 没有沙箱是**要你知道的事**（那些确认框是唯一的闸），
                    // 用琥珀；有沙箱是正常态，中性
                    color: _sandboxOk(health?.sandbox)
                        ? theme.colorScheme.onSurfaceVariant
                        : theme.cortex.warning,
                  ),
                  const SizedBox(width: 10),
                  Expanded(
                    child: SelectableText(
                      health?.sandbox ??
                          // 问不到就说问不到 —— 编一句「有沙箱」是这一页
                          // 最不该犯的错
                          '还没问到这台 agent 的沙箱状态（连上之后这里会说清楚）。',
                      style: theme.textTheme.bodySmall?.copyWith(height: 1.6),
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ],
    );
  }

  /// 这行状态描述算不算「有保护」。
  ///
  /// 判据是 agent 那句话里有没有「⚠」—— 它由 `cortex_agent::status_line_for`
  /// 生成，没有沙箱时必带这个前缀。**不在客户端重新推断一遍**：
  /// 推断要复制一份平台矩阵，而那份复制品迟早与真实执行环境对不上。
  bool _sandboxOk(String? line) => line != null && !line.contains('⚠');
}
