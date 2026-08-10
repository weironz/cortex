import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../auth/token_store.dart';
import '../../core/app_config.dart';
import '../../state/app_providers.dart';
import '../../state/auth_controller.dart';
import '../import/import_sheet.dart';

Future<void> showSettingsSheet(BuildContext context) {
  return showDialog<void>(
    context: context,
    builder: (_) => const _SettingsDialog(),
  );
}

/// Runtime data-source switch.
///
/// Changing either field rebuilds `cortexApiProvider`, which cascades through
/// the chat and memory controllers — no restart, and no stale data from the
/// previous backend.
class _SettingsDialog extends ConsumerStatefulWidget {
  const _SettingsDialog();

  @override
  ConsumerState<_SettingsDialog> createState() => _SettingsDialogState();
}

class _SettingsDialogState extends ConsumerState<_SettingsDialog> {
  late final TextEditingController _urlController = TextEditingController(
    text: ref.read(appConfigProvider).baseUrl,
  );

  @override
  void dispose() {
    _urlController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final config = ref.watch(appConfigProvider);
    final notifier = ref.read(appConfigProvider.notifier);
    final health = ref.watch(healthProvider);
    final auth = ref.watch(authControllerProvider);

    return AlertDialog(
      title: const Text('设置'),
      contentPadding: const EdgeInsets.fromLTRB(24, 16, 24, 0),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460),
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SwitchListTile.adaptive(
                contentPadding: EdgeInsets.zero,
                value: config.useMock,
                onChanged: notifier.setUseMock,
                title: const Text('使用 Mock 数据源'),
                subtitle: Text(
                  config.useMock
                      ? '不发起任何网络请求，数据来自内存夹具。'
                      : '直连 cortexd。',
                  style: theme.textTheme.bodySmall,
                ),
              ),
              const SizedBox(height: 8),
              TextField(
                controller: _urlController,
                enabled: !config.useMock,
                decoration: const InputDecoration(
                  labelText: 'cortexd 地址',
                  hintText: 'http://127.0.0.1:8080',
                ),
                onSubmitted: notifier.setBaseUrl,
              ),
              const SizedBox(height: 6),
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  onPressed: config.useMock
                      ? null
                      : () => notifier.setBaseUrl(_urlController.text),
                  child: const Text('应用地址'),
                ),
              ),
              const Divider(height: 24),
              Text('后端状态', style: theme.textTheme.titleSmall),
              const SizedBox(height: 8),
              health.when(
                loading: () => const Padding(
                  padding: EdgeInsets.symmetric(vertical: 6),
                  child: SizedBox(
                    width: 16,
                    height: 16,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  ),
                ),
                error: (e, _) => Text(
                  '$e',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.error,
                  ),
                ),
                data: (h) => Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    _kv(context, 'status', h.status),
                    _kv(context, 'version', h.version),
                    _kv(context, 'database', h.database),
                    _kv(context, 'auth', h.auth),
                    // Surfaced rather than quietly enjoyed. `cortexd` only
                    // reaches this state when someone wrote
                    // `CORTEX_AUTH=disabled` on purpose, and it warns on every
                    // start; a client that said nothing would be the only part
                    // of the system that failed to mention the memory store is
                    // open to anyone who can reach the port.
                    if (h.authDisabled && !config.useMock) ...[
                      const SizedBox(height: 6),
                      Row(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Icon(
                            Icons.lock_open_rounded,
                            size: 14,
                            color: scheme.error,
                          ),
                          const SizedBox(width: 6),
                          Expanded(
                            child: Text(
                              '这个 cortexd 关闭了认证：任何能连上 '
                              '${config.baseUrl} 的人都拥有全部记忆。'
                              '只有监听地址确实是回环时才可接受。',
                              style: theme.textTheme.labelSmall?.copyWith(
                                color: scheme.error,
                              ),
                            ),
                          ),
                        ],
                      ),
                    ],
                  ],
                ),
              ),
              if (!config.useMock && auth.token != null) ...[
                const Divider(height: 24),
                Row(
                  children: [
                    Expanded(
                      child: Text(
                        '已用 token 连接。凭据只存在于内存'
                        '${kCanRememberToken ? '（以及你勾选的 sessionStorage）' : '与环境变量 $kTokenEnvVar'}。',
                        style: theme.textTheme.bodySmall,
                      ),
                    ),
                    TextButton(
                      onPressed: () {
                        Navigator.of(context).pop();
                        unawaited(
                          ref.read(authControllerProvider.notifier).signOut(),
                        );
                      },
                      child: const Text('断开'),
                    ),
                  ],
                ),
              ],
              const Divider(height: 28),
              Text('数据', style: theme.textTheme.titleSmall),
              const SizedBox(height: 8),
              // Lives here, not in the toolbar: importing is something a person
              // does once. A permanent button for a one-time action is clutter,
              // and "数据" is where someone goes looking for it.
              ListTile(
                contentPadding: EdgeInsets.zero,
                leading: const Icon(Icons.download_outlined),
                title: const Text('导入 ChatGPT / Claude 历史'),
                subtitle: Text(
                  '选择导出包里的 conversations.json。会先把要花多少钱摊开，'
                  '确认之后才开始写。',
                  style: theme.textTheme.bodySmall,
                ),
                onTap: () {
                  // Close this one first. Stacking a long-running dialog on top
                  // of settings would leave the settings scrim behind a progress
                  // bar that runs for a quarter of an hour.
                  Navigator.of(context).pop();
                  unawaited(showImportSheet(context));
                },
              ),
              const SizedBox(height: 12),
              Text(
                '编译期默认值：USE_MOCK=${AppConfig.defaultUseMock}，'
                'CORTEX_BASE_URL=${AppConfig.defaultBaseUrl}',
                style: theme.textTheme.labelSmall,
              ),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => ref.invalidate(healthProvider),
          child: const Text('重新检测'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('完成'),
        ),
      ],
    );
  }

  Widget _kv(BuildContext context, String k, String v) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Row(
        children: [
          SizedBox(
            width: 80,
            child: Text(k, style: theme.textTheme.labelSmall),
          ),
          Text(
            v,
            style: theme.textTheme.bodySmall?.copyWith(
              fontFamily: 'monospace',
              color: theme.colorScheme.onSurface,
            ),
          ),
        ],
      ),
    );
  }
}
