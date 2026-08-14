import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api_exception.dart';
import '../../auth/token_store.dart';
import '../../auth/local_llm_store.dart';
import '../../core/app_config.dart';
import '../../core/local_agent.dart';
import '../../core/local_llm.dart';
import '../../models/llm_key_status.dart';
import '../../state/app_providers.dart';
import '../../state/auth_controller.dart';
import '../../state/update_controller.dart';
import '../../workspace/workspace_fs.dart';
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
                  config.useMock ? '不发起任何网络请求，数据来自内存夹具。' : '直连 cortexd。',
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
              // 「退出登录」搬去了左下角账号菜单。凭据存在哪这句话留着 ——
              // 它回答的是「我关掉这个窗口，密码会留在哪」，属于知情，
              // 不是一个操作
              if (!config.useMock && auth.token != null) ...[
                const Divider(height: 24),
                Text(
                  '已用 token 连接。凭据只存在于内存'
                  '${kCanRememberToken ? '（以及你勾选的 sessionStorage）' : '与环境变量 $kTokenEnvVar'}。',
                  style: theme.textTheme.bodySmall,
                ),
              ],
              const Divider(height: 28),
              Text('数据', style: theme.textTheme.titleSmall),
              const SizedBox(height: 8),
              const _WorkspaceRootTile(),
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
              const Divider(height: 28),
              Text('模型', style: theme.textTheme.titleSmall),
              const SizedBox(height: 8),
              // 配额超限的消息里写着「可以在设置里填自己的 API key」。
              // 那句话在这个入口存在之前就已经发到用户眼前了 ——
              // 一个正被拦住的人会来这里找它，而在此之前他什么也找不到
              const _OwnApiKeyTile(),
              const _LocalLlmTile(),
              const Divider(height: 28),
              Text('关于', style: theme.textTheme.titleSmall),
              const SizedBox(height: 8),
              const _AboutTile(),
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

/// 自带 API key 的那一格。
///
/// # 为什么不做成「输入框常驻」
///
/// 绝大多数人不需要它 —— 服务端那把 key 够用。常驻一个 API key 输入框
/// 会让每个打开设置的人以为「我是不是该填点什么」。所以默认只显示状态，
/// 点开才是输入。
class _OwnApiKeyTile extends ConsumerStatefulWidget {
  const _OwnApiKeyTile();

  @override
  ConsumerState<_OwnApiKeyTile> createState() => _OwnApiKeyTileState();
}

class _OwnApiKeyTileState extends ConsumerState<_OwnApiKeyTile> {
  LlmKeyStatus? _status;
  Object? _error;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    unawaited(_refresh());
  }

  Future<void> _refresh() async {
    try {
      final s = await ref.read(cortexApiProvider).llmKeyStatus();
      if (mounted) {
        setState(() {
          _status = s;
          _error = null;
        });
      }
    } on Object catch (e) {
      // 读不出来不该让整个设置页出错 —— 其余每一项都还是好的
      if (mounted) setState(() => _error = e);
    }
  }

  Future<void> _edit() async {
    final entered =
        await showDialog<({String provider, String key, String baseUrl})>(
          context: context,
          builder: (_) => const _ApiKeyDialog(),
        );
    if (entered == null || !mounted) return;
    setState(() => _busy = true);
    try {
      final s = await ref
          .read(cortexApiProvider)
          .setLlmKey(
            provider: entered.provider,
            apiKey: entered.key,
            baseUrl: entered.baseUrl,
          );
      if (mounted) {
        setState(() {
          _status = s;
          _error = null;
        });
      }
    } on Object catch (e) {
      if (mounted) setState(() => _error = e);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _clear() async {
    setState(() => _busy = true);
    try {
      final s = await ref.read(cortexApiProvider).clearLlmKey();
      if (mounted) {
        setState(() {
          _status = s;
          _error = null;
        });
      }
    } on Object catch (e) {
      if (mounted) setState(() => _error = e);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final st = _status;

    if (_error != null) {
      return ListTile(
        contentPadding: EdgeInsets.zero,
        leading: const Icon(Icons.key_outlined),
        title: const Text('自己的 API key'),
        subtitle: Text('读不出状态：$_error', style: theme.textTheme.bodySmall),
        trailing: TextButton(onPressed: _refresh, child: const Text('重试')),
      );
    }
    if (st == null) {
      return const ListTile(
        contentPadding: EdgeInsets.zero,
        leading: Icon(Icons.key_outlined),
        title: Text('自己的 API key'),
        subtitle: Text('读取中…'),
      );
    }
    // 这个部署存不了（没配主密钥）。**如实说** —— 给一个存不进去的
    // 输入框，用户会填、会点保存、会以为成了，下次打开又是空的
    if (!st.supported) {
      return ListTile(
        contentPadding: EdgeInsets.zero,
        leading: const Icon(Icons.key_off_outlined),
        title: const Text('自己的 API key'),
        subtitle: Text(
          '这个部署没有开启（服务端未配置 CORTEX_SECRET_KEY，存不了密钥）。'
          '所有调用走服务端那把 key，并计入配额。',
          style: theme.textTheme.bodySmall,
        ),
      );
    }

    return ListTile(
      contentPadding: EdgeInsets.zero,
      leading: Icon(st.configured ? Icons.key : Icons.key_outlined),
      title: const Text('自己的 API key'),
      subtitle: Text(
        st.configured
            ? '正在用你自己的 ${st.provider} key（…${st.keyTail}）'
                  '${st.baseUrl == null ? "" : " → ${st.baseUrl}"} —— 这部分调用不占配额。'
            : '填一把自己的 key，这之后的调用走你自己的账户，不占这里的配额。'
                  '明文只会在保存那一次发出去，服务端加密存储、之后只回后四位。',
        style: theme.textTheme.bodySmall,
      ),
      trailing: _busy
          ? const SizedBox(
              width: 18,
              height: 18,
              child: CircularProgressIndicator(strokeWidth: 2),
            )
          : Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                if (st.configured)
                  TextButton(onPressed: _clear, child: const Text('撤下')),
                TextButton(
                  onPressed: _edit,
                  child: Text(st.configured ? '换一把' : '填写'),
                ),
              ],
            ),
    );
  }
}

/// 填 key 的对话框。
class _ApiKeyDialog extends StatefulWidget {
  const _ApiKeyDialog();

  @override
  State<_ApiKeyDialog> createState() => _ApiKeyDialogState();
}

class _ApiKeyDialogState extends State<_ApiKeyDialog> {
  final _provider = TextEditingController(text: 'deepseek');
  final _key = TextEditingController();
  final _baseUrl = TextEditingController();
  bool _reveal = false;

  @override
  void dispose() {
    _provider.dispose();
    _key.dispose();
    _baseUrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('填自己的 API key'),
    content: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        TextField(
          controller: _provider,
          autocorrect: false,
          decoration: const InputDecoration(
            labelText: '供应商',
            hintText: 'deepseek / anthropic / …',
            border: OutlineInputBorder(),
          ),
        ),
        const SizedBox(height: 14),
        TextField(
          controller: _key,
          autofocus: true,
          autocorrect: false,
          enableSuggestions: false,
          // 默认遮住、可点开：这是一串粘进来的东西，而一次看不见的粘贴
          // 迟早会错一次，且症状要到下一次对话才出现
          obscureText: !_reveal,
          decoration: InputDecoration(
            labelText: 'API key',
            border: const OutlineInputBorder(),
            suffixIcon: IconButton(
              onPressed: () => setState(() => _reveal = !_reveal),
              iconSize: 18,
              icon: Icon(
                _reveal
                    ? Icons.visibility_off_outlined
                    : Icons.visibility_outlined,
              ),
            ),
          ),
        ),
        const SizedBox(height: 14),
        TextField(
          controller: _baseUrl,
          autocorrect: false,
          enableSuggestions: false,
          decoration: const InputDecoration(
            labelText: '端点（可选）',
            hintText: '留空 = 用官方的。自建 / 中转填这里',
            helperText: '公司网关、one-api / LiteLLM、自建 vLLM 都填这里',
            border: OutlineInputBorder(),
          ),
        ),
      ],
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.of(context).pop(),
        child: const Text('取消'),
      ),
      FilledButton(
        onPressed: () => Navigator.of(context).pop((
          provider: _provider.text.trim(),
          key: _key.text.trim(),
          baseUrl: _baseUrl.text.trim(),
        )),
        child: const Text('保存'),
      ),
    ],
  );
}

/// 本机模型配置 —— 离线模式那一格。
///
/// # 为什么与上面那格并排，而不是合成一个
///
/// 它们回答的是两个不同的问题：
///
/// - 「自己的 API key」= **服务器**用哪把 key（存进 cortexd、跟着账号走）
/// - 「本机模型」= **这台电脑离线时**打给谁（存进系统凭据库、不上传）
///
/// 合成一格会让「我明明填过 key 了怎么离线还是用不了」成为常态，
/// 而那时用户找不到任何线索。并排且各自说清用途，是这里唯一诚实的做法。
/// 默认工作空间存储路径。
///
/// # 为什么它显示的路径**可能还不存在**
///
/// 没设过时 agent 回的是建议值（`~/Cortex`），磁盘上要到第一次真用到才落地。
/// 照样显示它，因为这一栏回答的问题是「我的文件会去哪儿」——「还没建」不是
/// 这个问题的答案。
class _WorkspaceRootTile extends ConsumerWidget {
  const _WorkspaceRootTile();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    // Web 上没有本机目录可言：那边的 agent 在云端容器里，工作区是它自己的
    // `/workspace`。判据用能力而不是 kIsWeb，与 WorkspaceChip 同一个
    if (!kLocalAgentSupported) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final async = ref.watch(localWorkspaceRootProvider);
    final root = async.value?.root;

    return ListTile(
      contentPadding: EdgeInsets.zero,
      leading: const Icon(Icons.folder_special_outlined),
      title: const Text('默认工作空间'),
      subtitle: Text(
        async.isLoading
            ? '读取中…'
            : root == null
            ? '本地 agent 没在跑，读不到这台机器上的设置。'
            : '$root\n新建对话时会在这里建文件夹；改了不影响已有数据。',
        style: theme.textTheme.bodySmall,
      ),
      isThreeLine: root != null,
      trailing: TextButton(
        onPressed: root == null ? null : () => _change(context, ref),
        child: const Text('更改'),
      ),
    );
  }

  Future<void> _change(BuildContext context, WidgetRef ref) async {
    final picked = await pickWorkspaceDirectory();
    if (picked == null || !context.mounted) return;
    try {
      await ref.read(cortexApiProvider).setLocalWorkspaceRoot(picked);
      ref.invalidate(localWorkspaceRootProvider);
    } on CortexApiException catch (e) {
      if (!context.mounted) return;
      // 校验器会拒掉主目录本身与系统目录，理由是写给人看的，原样显示
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(e.message)));
    }
  }
}

class _LocalLlmTile extends ConsumerWidget {
  const _LocalLlmTile();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    if (!kCanStoreLocalLlm) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final offline = ref.watch(appConfigProvider.select((c) => c.offline));
    final async = ref.watch(localLlmProvider);
    final cfg = async.value ?? LocalLlmConfig.empty;

    final subtitle = switch ((async.isLoading, cfg.isUsable)) {
      (true, _) => '读取中…',
      (_, true) =>
        '离线时用 ${cfg.provider}'
            '${cfg.model.isEmpty ? "" : " · ${cfg.model}"}'
            '${cfg.baseUrl.isEmpty ? "" : " → ${cfg.baseUrl}"}',
      _ =>
        '离线模式要用它 —— 没有 cortexd 就没有可代理的对象，'
            '本地 agent 必须自己知道打给谁。只存在这台电脑的系统凭据库里。',
    };

    return ListTile(
      contentPadding: EdgeInsets.zero,
      leading: Icon(
        cfg.isUsable ? Icons.computer : Icons.computer_outlined,
        // 离线模式下没配 = 用不了，这时候才需要把它标红
        color: offline && !cfg.isUsable ? theme.colorScheme.error : null,
      ),
      title: const Text('本机模型（离线时用）'),
      subtitle: Text(subtitle, style: theme.textTheme.bodySmall),
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (cfg.isUsable)
            TextButton(
              onPressed: () => ref.read(localLlmProvider.notifier).clear(),
              child: const Text('清除'),
            ),
          TextButton(
            onPressed: () async {
              final entered = await showDialog<LocalLlmConfig>(
                context: context,
                builder: (_) => _LocalLlmDialog(initial: cfg),
              );
              if (entered == null) return;
              await ref.read(localLlmProvider.notifier).save(entered);
            },
            child: Text(cfg.isUsable ? '修改' : '配置'),
          ),
        ],
      ),
    );
  }
}

class _LocalLlmDialog extends StatefulWidget {
  const _LocalLlmDialog({required this.initial});

  final LocalLlmConfig initial;

  @override
  State<_LocalLlmDialog> createState() => _LocalLlmDialogState();
}

class _LocalLlmDialogState extends State<_LocalLlmDialog> {
  late final _provider = TextEditingController(text: widget.initial.provider);
  late final _model = TextEditingController(text: widget.initial.model);
  late final _baseUrl = TextEditingController(text: widget.initial.baseUrl);
  // key **不回填**：它存在凭据库里，读出来放进一个输入框等于把它又摊开
  // 一次。想换就重填，想保持不动就留空
  final _key = TextEditingController();
  bool _reveal = false;

  @override
  void dispose() {
    _provider.dispose();
    _model.dispose();
    _baseUrl.dispose();
    _key.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return AlertDialog(
      title: const Text('本机模型'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              '离线模式下本地 agent 直连这个端点。配置只存在这台电脑上，'
              '不会上传到任何地方。',
              style: theme.textTheme.bodySmall,
            ),
            const SizedBox(height: 14),
            TextField(
              controller: _provider,
              autocorrect: false,
              autofocus: true,
              decoration: const InputDecoration(
                labelText: '供应商',
                hintText: 'ollama / deepseek / openai …',
                helperText: '本机 ollama 免 key；其余按各家的 key 填下面',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 14),
            TextField(
              controller: _model,
              autocorrect: false,
              decoration: const InputDecoration(
                labelText: '模型（可选）',
                hintText: '留空 = 用供应商的默认模型',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 14),
            TextField(
              controller: _baseUrl,
              autocorrect: false,
              decoration: const InputDecoration(
                labelText: '端点（可选）',
                hintText: 'http://127.0.0.1:11434 …',
                helperText: '留空 = 用供应商的官方地址',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 14),
            TextField(
              controller: _key,
              obscureText: !_reveal,
              autocorrect: false,
              enableSuggestions: false,
              decoration: InputDecoration(
                labelText: 'API key（可选）',
                hintText: widget.initial.apiKey.isEmpty
                    ? '免鉴权的端点留空'
                    : '留空 = 不改动已存的那把',
                border: const OutlineInputBorder(),
                suffixIcon: IconButton(
                  onPressed: () => setState(() => _reveal = !_reveal),
                  iconSize: 18,
                  icon: Icon(
                    _reveal
                        ? Icons.visibility_off_outlined
                        : Icons.visibility_outlined,
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(
            LocalLlmConfig(
              provider: _provider.text.trim(),
              model: _model.text.trim(),
              baseUrl: _baseUrl.text.trim(),
              // 留空 = 保留原来那把，不是清空。清空请用外面那个「清除」
              apiKey: _key.text.trim().isEmpty
                  ? widget.initial.apiKey
                  : _key.text.trim(),
            ),
          ),
          child: const Text('保存'),
        ),
      ],
    );
  }
}

/// 版本号 + 手动检查更新的入口。
///
/// 顶栏那个小图标是常驻的、安静的那一半；这里是「我特地来找它」的那一半。
/// 两个入口指向同一个 controller —— 一个功能有两个各自的状态是本仓库
/// 已经吃过几次亏的形状。
class _AboutTile extends ConsumerWidget {
  const _AboutTile();

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
