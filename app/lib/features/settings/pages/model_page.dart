import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../auth/local_llm_store.dart';
import '../../../core/local_llm.dart';
import '../../../models/llm_key_status.dart';
import '../../../state/app_providers.dart';

/// 模型这一页：服务端那把 key 之外的两个出口。
///
/// 「自己的 key」与「本机模型」放在一起，是因为它们回答的是同一个问题
/// —— **这一轮由谁付钱、跑在哪** —— 只是答案一个在云上一个在本机。
class ModelPage extends StatelessWidget {
  const ModelPage({super.key});

  @override
  Widget build(BuildContext context) => ListView(
    padding: const EdgeInsets.symmetric(horizontal: 4),
    children: const [
      // 配额超限的消息里写着「可以在设置里填自己的 API key」。那句话在这个
      // 入口存在之前就已经发到用户眼前了 —— 一个正被拦住的人会来这里找它
      OwnApiKeyTile(),
      LocalLlmTile(),
    ],
  );
}

/// 自带 API key 的那一格。
///
/// # 为什么不做成「输入框常驻」
///
/// 绝大多数人不需要它 —— 服务端那把 key 够用。常驻一个 API key 输入框
/// 会让每个打开设置的人以为「我是不是该填点什么」。所以默认只显示状态，
/// 点开才是输入。
class OwnApiKeyTile extends ConsumerStatefulWidget {
  const OwnApiKeyTile({super.key});

  @override
  ConsumerState<OwnApiKeyTile> createState() => OwnApiKeyTileState();
}

class OwnApiKeyTileState extends ConsumerState<OwnApiKeyTile> {
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
class LocalLlmTile extends ConsumerWidget {
  const LocalLlmTile({super.key});

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
