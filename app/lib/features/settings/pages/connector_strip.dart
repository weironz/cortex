/// 「精选连接器」那一条 —— 列表页顶上的一排一键接入。
///
/// # 它接出来的就是一台普通的 MCP server
///
/// 同一份 `.mcp.json`、同一个列表、同一个详情页、同一条删除路径。这里省掉的
/// 只是「叫什么包、参数怎么写」那一步 —— 那一步是绝大多数人卡住的地方，
/// 而它跟能力本身毫无关系。
///
/// # ⚠️ 已经接过的那条要**看得出来**
///
/// 不标的话，用户会再点一次，而 `PUT` 是覆盖 —— 他那台配好的（改过 trust、
/// 填过 key）会被这份默认配置盖掉，而界面上什么都不会发生。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/theme.dart';
import '../../../models/mcp.dart';
import '../../../state/app_providers.dart';
import 'connector_presets.dart';

class ConnectorStrip extends ConsumerStatefulWidget {
  const ConnectorStrip({super.key, required this.config});

  final McpConfigView config;

  @override
  ConsumerState<ConnectorStrip> createState() => _ConnectorStripState();
}

class _ConnectorStripState extends ConsumerState<ConnectorStrip> {
  /// 正在接哪一条。同时只让接一条 —— 两条一起 `npx` 会互相抢 npm 缓存锁，
  /// 表现是其中一条莫名其妙地失败
  String? _busy;

  bool _installed(ConnectorPreset p) =>
      widget.config.servers.any((s) => s.name == p.name);

  Future<void> _install(ConnectorPreset p) async {
    // 在**任何** await 之前抓住它：问 key 那一步会跨越一个 async gap，
    // 之后这个 State 可能已经不在树上了（用户关掉了设置）
    final messenger = ScaffoldMessenger.maybeOf(context);
    var env = const <String, String>{};
    if (p.envHint case final hint?) {
      // ⚠️ key 只经过**用户的键盘**：我们既不代填也不代存。填完它进的是本机
      // 那份 .mcp.json，而服务端读它时只回名字不回值
      final got = await _askForKey(context, p, hint);
      if (got == null || !mounted) return;
      env = {hint.variable: got};
    }
    setState(() => _busy = p.id);
    try {
      await ref
          .read(cortexApiProvider)
          .saveMcpServer(
            name: p.name,
            config: {...p.config, if (env.isNotEmpty) 'env': env},
          );
      ref.invalidate(mcpConfigProvider);
    } on CortexApiException catch (e) {
      messenger?.showSnackBar(SnackBar(content: Text(e.message)));
    } finally {
      if (mounted) setState(() => _busy = null);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('精选', style: theme.textTheme.labelMedium),
        const SizedBox(height: 2),
        Text(
          // 说清「接上之后它跟手动加的那些没有区别」—— 不说的话，用户会以为
          // 这是一类特殊的、不好删的东西
          '一键接上，之后它就是下面列表里的一台，随时可以改或删。第一次连接要等 npx 下载。',
          style: theme.textTheme.labelSmall?.copyWith(
            color: theme.cortex.foregroundTertiary,
          ),
        ),
        const SizedBox(height: 8),
        Wrap(
          spacing: 8,
          runSpacing: 8,
          children: [
            for (final p in connectorPresets)
              _PresetChip(
                preset: p,
                installed: _installed(p),
                busy: _busy == p.id,
                // 已经接过的不给点：`PUT` 是覆盖，会把用户改过的 trust
                // 与填过的 key 一起盖掉，而界面上什么都不会发生
                onTap: _busy != null || _installed(p)
                    ? null
                    : () => _install(p),
              ),
          ],
        ),
      ],
    );
  }
}

class _PresetChip extends StatelessWidget {
  const _PresetChip({
    required this.preset,
    required this.installed,
    required this.busy,
    required this.onTap,
  });

  final ConnectorPreset preset;
  final bool installed;
  final bool busy;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Tooltip(
      message: installed ? '已经接上了 —— 在下面那份列表里管理它' : preset.description,
      child: Material(
        color: installed
            ? scheme.surfaceContainerHigh
            : scheme.surfaceContainer,
        clipBehavior: Clip.antiAlias,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
          side: BorderSide(color: scheme.outlineVariant),
        ),
        child: InkWell(
          key: ValueKey('connector:${preset.id}'),
          onTap: onTap,
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(preset.icon, style: const TextStyle(fontSize: 15)),
                const SizedBox(width: 7),
                Text(preset.title, style: theme.textTheme.bodySmall),
                const SizedBox(width: 7),
                if (busy)
                  const SizedBox(
                    width: 13,
                    height: 13,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                else if (installed)
                  Icon(
                    Icons.check_rounded,
                    size: 14,
                    color: theme.cortex.foregroundTertiary,
                  )
                else ...[
                  if (preset.needsKey)
                    // 要填 key 的那条提前说 —— 点下去才发现要一把令牌是最糟的
                    Icon(
                      Icons.key_outlined,
                      size: 13,
                      color: theme.cortex.foregroundTertiary,
                    ),
                  Icon(Icons.add_rounded, size: 14, color: scheme.primary),
                ],
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// 问用户要那把 key。
///
/// ⚠️ **必须说清去哪儿拿。** 一个只写着 `GITHUB_PERSONAL_ACCESS_TOKEN`
/// 的输入框回答不了「我上哪儿弄一个」，而那正是用户在这一步唯一的问题。
Future<String?> _askForKey(
  BuildContext context,
  ConnectorPreset preset,
  ConnectorEnvHint hint,
) async {
  final controller = TextEditingController();
  final got = await showDialog<String>(
    context: context,
    builder: (ctx) {
      final theme = Theme.of(ctx);
      return AlertDialog(
        title: Text('接入「${preset.title}」'),
        content: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 460),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(preset.description, style: theme.textTheme.bodySmall),
              const SizedBox(height: 14),
              TextField(
                key: const ValueKey('connector:key'),
                controller: controller,
                autofocus: true,
                obscureText: true,
                decoration: InputDecoration(
                  labelText: hint.label,
                  helperText: hint.where,
                  helperMaxLines: 3,
                ),
                onSubmitted: (v) => Navigator.of(ctx).pop(v.trim()),
              ),
              const SizedBox(height: 10),
              Text(
                // 说清它存在哪儿：一个要 key 的对话框不交代去向，
                // 谨慎的用户会直接放弃
                '它只写进这台机器上的 .mcp.json，不会上传。'
                '之后界面上只显示变量名，不显示值。',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                ),
              ),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            key: const ValueKey('connector:key:ok'),
            onPressed: () => Navigator.of(ctx).pop(controller.text.trim()),
            child: const Text('接入'),
          ),
        ],
      );
    },
  );
  controller.dispose();
  return (got?.isEmpty ?? true) ? null : got;
}
