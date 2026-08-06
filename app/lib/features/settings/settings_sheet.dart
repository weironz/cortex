import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/app_config.dart';
import '../../state/app_providers.dart';

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
                  ],
                ),
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
