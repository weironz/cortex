import 'model_picker.dart';
import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../auth/local_llm_store.dart';
import '../../../core/local_llm.dart';
import '../../../models/llm_key_status.dart';
import '../../../state/app_providers.dart';
import '../../../state/model_controller.dart';

/// 模型这一页：**三个问题，各一节**。
///
/// # 为什么要分节
///
/// 原先这一页是一串平铺的选项：六个单选按钮（跟随部署 / 自动 / 四个型号），
/// 紧接着「自己的 API key」，再接着「本机模型」。用户的原话是
/// 「上来就给四种选择，太难理解了」。
///
/// 他没看错 —— 那四项**看起来并列，实际回答三个不同问题**：
///
/// | 问题 | 谁回答 |
/// |---|---|
/// | 用哪个模型 | 跟随部署 / 自动 / 指定一个 |
/// | 谁的账户付这笔钱 | 服务端那把 key，还是你自己的 |
/// | 没有网的时候打给谁 | 本机模型（只有桌面端有这一节） |
///
/// 平铺的问题不在「选项多」，而在**读者无从知道哪些是互斥的**：
/// 「自己的 API key」看着像第四个模型选项，可它跟选哪个模型毫无关系。
/// 分节之后，同一节里的东西互斥、跨节的东西正交，这件事不用解释就看得出来。
///
/// 顺带把第一节收成一行：铺开六个单选项会让它的体积是另外两节的三倍，
/// 于是它像「主菜单」而后两节像它的下级。换模型的主入口现在也不在这儿 ——
/// 输入框下面那个 chip 才是（见 `ModelChip`）。
class ModelPage extends StatelessWidget {
  const ModelPage({super.key});

  @override
  Widget build(BuildContext context) => ListView(
    padding: const EdgeInsets.symmetric(horizontal: 4),
    children: const [
      _Section(
        title: '用哪个模型',
        blurb: '这个选择逐轮生效，下一句就按新的走。',
        child: ModelPickerTile(),
      ),
      _Section(
        title: '谁的账户付这笔钱',
        // 配额超限的消息里写着「可以在设置里填自己的 API key」。那句话在这个
        // 入口存在之前就已经发到用户眼前了 —— 一个正被拦住的人会来这里找它
        blurb: '默认走服务端那把 key，并计入这里的配额。',
        child: OwnApiKeyTile(),
      ),
      // 这一节 Web 上整节不显示 —— 浏览器里没有本地 agent，
      // 这份配置存了也没有任何东西会读它（见 `local_llm_store_web.dart`）。
      // **不是「Web 功能少一块」**，是那里根本不存在这个问题
      _Section(
        title: '没有网的时候打给谁',
        blurb: '离线模式下本地 agent 直连你填的端点，配置只存在这台电脑上。',
        child: LocalLlmTile(),
      ),
    ],
  );
}

/// 一节：标题 + 一句话说清这一节在回答什么 + 内容。
///
/// 内容为空（例如 Web 上的「本机模型」）时**整节消失** ——
/// 留一个空标题会让人以为这里坏了。
class _Section extends StatelessWidget {
  const _Section({
    required this.title,
    required this.blurb,
    required this.child,
  });

  final String title;
  final String blurb;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    if (!_visible(child)) return const SizedBox.shrink();
    return Padding(
      padding: const EdgeInsets.only(bottom: 22),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            title,
            style: theme.textTheme.titleSmall?.copyWith(
              fontWeight: FontWeight.w700,
            ),
          ),
          const SizedBox(height: 2),
          Text(
            blurb,
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
          const SizedBox(height: 6),
          child,
        ],
      ),
    );
  }

  /// 这一节有没有内容。目前只有「本机模型」会整节缺席（Web）。
  static bool _visible(Widget child) =>
      child is! LocalLlmTile || kCanStoreLocalLlm;
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
          builder: (_) => _ApiKeyDialog(
            providers: _status?.providers ?? const [],
            initial: _status?.provider,
          ),
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
            ? '你自己的 ${st.provider} key（…${st.keyTail}）'
                  '${st.baseUrl == null ? "" : " → ${st.baseUrl}"} —— 这部分调用不占配额。'
            // 「明文只发一次」这句必须留着：它是用户决定要不要填的依据，
            // 而分节的标题回答不了「我的 key 会被怎么处理」
            : '填一把自己的，调用就走你自己的账户。明文只在保存那一次发出去，'
                  '服务端加密存储、之后只回后四位。',
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
///
/// # 供应商是下拉，不是输入框
///
/// 服务端只认它自己那份清单里的名字（`byo_key` 保存时逐字校验）。
/// 让人手打的话，能填对完全靠他猜中我们内部的 id —— `claude` 不行要写
/// `anthropic`、`gemini` 不行要写 `google`、`kimi` 不行要写 `moonshot`。
/// 猜错的反馈来得也晚：填完、点保存，才收到一句「不认识的供应商」。
///
/// 清单是服务端下发的（`providers`），不是客户端硬编一份 ——
/// 硬编的那份迟早与校验那份分叉，而分叉的表现是「下拉里选得到、保存时被拒」。
class _ApiKeyDialog extends StatefulWidget {
  const _ApiKeyDialog({required this.providers, this.initial});

  final List<ProviderChoice> providers;

  /// 已经填过的那家，回填用。
  final String? initial;

  @override
  State<_ApiKeyDialog> createState() => _ApiKeyDialogState();
}

class _ApiKeyDialogState extends State<_ApiKeyDialog> {
  late String? _picked = _initialPick();
  final _key = TextEditingController();
  final _baseUrl = TextEditingController();
  bool _reveal = false;

  /// 老服务端不下发 `providers`。那时退回手打 —— 一个空下拉比输入框更糟，
  /// 它让人**完全无法**填，而这个功能在那种部署上其实是好的
  bool get _manual => widget.providers.isEmpty;
  final _manualProvider = TextEditingController();

  String? _initialPick() {
    if (widget.providers.isEmpty) return null;
    final want = widget.initial;
    if (want != null && widget.providers.any((p) => p.id == want)) return want;
    return widget.providers.first.id;
  }

  ProviderChoice? get _current {
    for (final p in widget.providers) {
      if (p.id == _picked) return p;
    }
    return null;
  }

  @override
  void initState() {
    super.initState();
    _manualProvider.text = widget.initial ?? '';
  }

  @override
  void dispose() {
    _key.dispose();
    _baseUrl.dispose();
    _manualProvider.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final cur = _current;
    // 免鉴权的（本机 ollama）不该逼人填 key
    final needsKey = cur?.requiresAuth ?? true;

    return AlertDialog(
      title: const Text('填自己的 API key'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (_manual)
              TextField(
                controller: _manualProvider,
                autocorrect: false,
                decoration: const InputDecoration(
                  labelText: '供应商',
                  hintText: 'deepseek / anthropic / …',
                  helperText: '这个部署没有下发可选清单（服务端版本较早）',
                  border: OutlineInputBorder(),
                ),
              )
            else
              DropdownButtonFormField<String>(
                initialValue: _picked,
                isExpanded: true,
                decoration: const InputDecoration(
                  labelText: '供应商',
                  border: OutlineInputBorder(),
                ),
                items: [
                  for (final p in widget.providers)
                    DropdownMenuItem(
                      value: p.id,
                      child: Text(
                        p.displayName,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                ],
                onChanged: (v) => setState(() => _picked = v),
              ),
            if (cur != null && cur.description.isNotEmpty) ...[
              const SizedBox(height: 6),
              Text(cur.description, style: theme.textTheme.labelSmall),
            ],
            const SizedBox(height: 14),
            if (needsKey)
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
              )
            else
              Text(
                '${cur?.displayName ?? "这一家"}不需要密钥，直接保存即可。',
                style: theme.textTheme.bodySmall,
              ),
            const SizedBox(height: 14),
            TextField(
              controller: _baseUrl,
              autocorrect: false,
              enableSuggestions: false,
              decoration: InputDecoration(
                labelText: '端点（可选）',
                // 说得出留空会连到哪 —— 「用官方的」这四个字回答不了
                // 「官方的是哪个」，而那正是想改端点的人要核对的东西
                hintText: cur == null ? '留空 = 用官方的' : '留空 = ${cur.baseUrl}',
                helperText: '公司网关、one-api / LiteLLM、自建 vLLM 都填这里',
                border: const OutlineInputBorder(),
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
          onPressed: () => Navigator.of(context).pop((
            provider: _manual ? _manualProvider.text.trim() : (_picked ?? ''),
            key: _key.text.trim(),
            baseUrl: _baseUrl.text.trim(),
          )),
          child: const Text('保存'),
        ),
      ],
    );
  }
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
      // 节标题已经说过「配置只存在这台电脑上」，这里只说**没配的后果**
      _ => '还没配 —— 断网时这台机器发不出消息。',
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
                builder: (_) => _LocalLlmDialog(
                  initial: cfg,
                  providers:
                      ref.read(providerChoicesProvider).value ?? const [],
                ),
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
  const _LocalLlmDialog({required this.initial, required this.providers});

  final LocalLlmConfig initial;

  /// 可选的供应商。空 = 服务端没下发（老版本或离线），退回手打。
  final List<ProviderChoice> providers;

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
            // 与上面那个对话框同一份清单、同一个理由：服务端只认它自己
            // 那些 id，手打就是在猜（`kimi` 要写 `moonshot`）。
            // 清单拉不到时（老服务端 / 离线）退回输入框 ——
            // 本机模型这一格在离线时正是最需要能用的那个
            if (widget.providers.isEmpty)
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
              )
            else
              DropdownButtonFormField<String>(
                initialValue:
                    widget.providers.any((p) => p.id == _provider.text)
                    ? _provider.text
                    : widget.providers.first.id,
                isExpanded: true,
                decoration: const InputDecoration(
                  labelText: '供应商',
                  helperText: '本机 ollama 免 key；其余按各家的 key 填下面',
                  border: OutlineInputBorder(),
                ),
                items: [
                  for (final p in widget.providers)
                    DropdownMenuItem(
                      value: p.id,
                      child: Text(
                        p.displayName,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                ],
                onChanged: (v) => setState(() => _provider.text = v ?? ''),
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
