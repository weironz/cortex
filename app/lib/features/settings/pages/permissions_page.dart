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
import '../../../state/sandbox_backend_controller.dart';
import 'dart:io' show Platform;
import 'package:flutter/foundation.dart' show kIsWeb;
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
          description:
              '新会话从哪一档开始。对话里那颗 chip 仍然能临时改这一轮 —— '
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
        if (_showBackendChoice) _backendSection(context, ref),
      ],
    );
  }

  /// 只在 **Windows 桌面端**画后端选择：别的平台没有这两档，Web 没有本机
  /// agent。判据放在这里而不是 provider 里 —— 它是纯平台事实，不必绕一层。
  bool get _showBackendChoice => !kIsWeb && Platform.isWindows;

  Widget _backendSection(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final restricted = ref.watch(sandboxBackendProvider);
    return SettingsSection(
      title: 'Windows 沙箱后端',
      description: '两档是两种取舍，不是强弱。换档会重启本机 agent。',
      children: [
        SettingsCard(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SwitchListTile(
                key: const ValueKey('sandbox:restricted'),
                contentPadding: EdgeInsets.zero,
                value: restricted,
                onChanged: (v) =>
                    ref.read(sandboxBackendProvider.notifier).setRestricted(v),
                title: const Text('受限令牌档（完整工具链）'),
                subtitle: Text(
                  '开：cargo 拉依赖 / git / curl 全通，能跑完整构建。'
                  '关（默认）：AppContainer，读默认拒绝、能防不受信代码偷读，'
                  '但 dir/vol 与含 cl.exe 的 C/C++ 构建跑不了。',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: theme.cortex.foregroundTertiary,
                    height: 1.5,
                  ),
                ),
              ),
              if (restricted)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Icon(
                        Icons.gpp_maybe_outlined,
                        size: 16,
                        color: theme.cortex.warning,
                      ),
                      const SizedBox(width: 8),
                      Expanded(
                        child: Text(
                          // 这一档最要紧的一句，必须摆在开着的时候：不挡读。
                          '这一档不挡读 —— 沙箱里的命令读得到你能读的任何文件'
                          '（写仍只限工作区）。要防不受信代码偷读，用默认那档。',
                          style: theme.textTheme.labelSmall?.copyWith(
                            color: theme.cortex.warning,
                            height: 1.5,
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
            ],
          ),
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
