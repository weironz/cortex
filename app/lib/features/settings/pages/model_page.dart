/// 模型这一页 —— **一份来源列表，第一步就是「添加模型」**。
///
/// # 它取代了什么
///
/// 从前这里平铺着三样东西：「跟随部署」六个单选项、「自己的 API key」、
/// 「本机模型」。用户的原话是「上来就给四种选择，太难理解了」，接着是
/// 「第一步都是添加模型，不分云端本地，离线在线」。
///
/// 那三样结构上本来就是同一个东西（`{供应商, key, 端点}`），只是**存储
/// 位置**不同。位置是我们的实现细节，不该由用户来分类。
///
/// 形态参照 Cherry Studio 的「模型服务」：左边一列来源，右边是选中那条的
/// 详情（密钥 / 端点 / 型号），型号靠「获取模型列表」向供应商实拉。
///
/// # 部署提供的那条也在列表里
///
/// 它只读、不可删（`builtin`），但**能关**。藏起来的话，一个零配置就能聊
/// 的新用户会以为自己什么都没有；而不给关，一个自带 key 的人就没办法
/// 阻止某些对话去花服务端的钱。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../auth/local_llm_store.dart';
import '../../../core/local_llm.dart';
import '../../../models/model_source.dart';
import '../../../state/app_providers.dart';
import '../../../state/model_controller.dart';

class ModelPage extends ConsumerStatefulWidget {
  const ModelPage({super.key});

  @override
  ConsumerState<ModelPage> createState() => _ModelPageState();
}

class _ModelPageState extends ConsumerState<ModelPage> {
  /// 选中的那条。`null` = 还没选，显示第一条。
  String? _selected;
  bool _busy = false;
  Object? _error;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final async = ref.watch(modelSourcesProvider);

    return async.when(
      // ⚠️ **重拉时不要退回转圈**：认证落定时 `cortexApiProvider` 会重建一次，
      // 那时 Riverpod 给的是「带着上一次结果的 AsyncLoading」，朴素的 `.when`
      // 会匹配到 loading —— 表现是每次都闪一下空列表
      skipLoadingOnReload: true,
      skipLoadingOnRefresh: true,
      loading: () => const Center(child: CircularProgressIndicator()),
      error: (e, _) {
        // 老服务端没有这条路。那不是错误，也不是用户能处理的事
        if (e is CortexApiException && e.isUnsupported) {
          return _hint('这个部署还不支持管理模型来源（服务端版本较早），用的是它自己配的那个。');
        }
        return _hint(
          e is CortexApiException ? e.message : '$e',
          onRetry: () => ref.invalidate(modelSourcesProvider),
        );
      },
      data: (data) {
        final list = data.sources;
        final current =
            data.byId(_selected ?? '') ?? (list.isEmpty ? null : list.first);
        return Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (_error != null)
              Padding(
                padding: const EdgeInsets.only(bottom: 8),
                child: Text(
                  '$_error',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.error,
                  ),
                ),
              ),
            Expanded(
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  SizedBox(
                    width: 190,
                    child: _SourceList(
                      data: data,
                      selected: current?.id,
                      busy: _busy,
                      onSelect: (id) => setState(() => _selected = id),
                      onAdd: _add,
                      onToggle: _toggle,
                    ),
                  ),
                  const VerticalDivider(width: 20),
                  Expanded(
                    child: current == null
                        ? _hint('还没有任何模型来源。点左边的「添加模型」开始。')
                        : _Detail(
                            key: ValueKey(current.id),
                            data: data,
                            source: current,
                            busy: _busy,
                            onEdit: () => _edit(current),
                            onDelete: () => _delete(current),
                            onFetch: () => _fetch(current),
                            onModels: (m) => _saveModels(current, m),
                            offlineNote: _offlineNote(current),
                          ),
                  ),
                ],
              ),
            ),
          ],
        );
      },
    );
  }

  Widget _hint(String text, {VoidCallback? onRetry}) => Padding(
    padding: const EdgeInsets.all(12),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(text, style: Theme.of(context).textTheme.bodySmall),
        if (onRetry != null)
          TextButton(onPressed: onRetry, child: const Text('重试')),
      ],
    ),
  );

  /// 跑一次会改动来源的操作，然后重拉列表。
  ///
  /// 统一在这里 catch：每处各写一遍的话，迟早有一处把错误吞了，
  /// 而症状是「点了保存，什么都没发生」。
  Future<void> _run(Future<void> Function() body) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await body();
      ref.invalidate(modelSourcesProvider);
      // 型号列表变了，选择器也要重拉
      ref.invalidate(modelCatalogProvider);
    } on Object catch (e) {
      if (mounted) {
        setState(() => _error = e is CortexApiException ? e.message : e);
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _add() async {
    final data = ref.read(modelSourcesProvider).value;
    if (data == null) return;
    final got = await showDialog<_SourceForm>(
      context: context,
      builder: (_) => _SourceDialog(providers: data.providers),
    );
    if (got == null) return;
    await _run(
      () => ref
          .read(cortexApiProvider)
          .saveModelSource(
            provider: got.provider,
            apiKey: got.apiKey,
            label: got.label,
            baseUrl: got.baseUrl,
          ),
    );
    await _mirrorOffline(got);
  }

  /// 这条来源在离线那一侧是什么状态。
  ///
  /// 三种，每一种都对应一件用户看不见但会咬到他的事：
  ///
  /// | 说什么 | 为什么必须说 |
  /// |---|---|
  /// | 「离线时用它」 | 他要知道断网之后跑的是哪一条、花的是哪把 key |
  /// | 「本机没有这把 key」 | 在另一台设备上加的来源，这台机器离线用不了 ——   ///   而界面上其余地方看起来它完全正常 |
  /// | Web / 部署那条 → `null` | 那里没有离线这回事，说了只是噪音 |
  String? _offlineNote(ModelSource s) {
    if (!kCanStoreLocalLlm || s.builtin) return null;
    final local = ref.watch(localLlmProvider).value;
    if (local != null && local.isUsable && local.provider == s.provider) {
      return '断网时用的就是它（密钥在这台电脑的系统凭据库里）。';
    }
    return '本机没有这把密钥，所以断网时用不了它 —— '
        '密钥从不明文离开服务端，只有你在这台机器上亲手填过的那次才会留一份。'
        '想让它能离线用：点「编辑」把密钥重填一遍。';
  }

  /// 顺手在这台机器上留一份，**离线时就是它**。
  ///
  /// # 为什么只能在这里做
  ///
  /// 客户端**永远拿不到明文 key** —— 服务端只回后四位。所以「用户刚输入的
  /// 那一刻」是唯一有明文的时刻，错过就再也拿不到了。
  ///
  /// 代价必须说清楚（界面上也写着）：**在另一台设备上加的来源，这台机器
  /// 离线时用不了**。那不是漏做，是密钥从不明文离开服务端的直接结果。
  ///
  /// # 为什么不问用户
  ///
  /// 他的原话是「不分云端本地，离线在线」。多一个「要不要也存在本机」的
  /// 勾选框，就是又让他分类一次。默认存 —— 密钥进的是系统凭据库，
  /// 与从前那个「本机模型」同一个地方，不是新增的风险面。
  Future<void> _mirrorOffline(_SourceForm form) async {
    // Web 上没有本地 agent，存了也没有任何东西会读它
    if (!kCanStoreLocalLlm) return;
    // 编辑时留空 = 不改动服务端那把，那我们手上也就没有明文可存 ——
    // 这时**不要动**本机那份：覆盖成空的话，离线会从「能用」变成「不能用」，
    // 而用户只是改了个标签
    if (form.apiKey.isEmpty) return;
    try {
      await ref
          .read(localLlmProvider.notifier)
          .save(
            LocalLlmConfig(
              provider: form.provider,
              apiKey: form.apiKey,
              baseUrl: form.baseUrl,
              // 型号这时候多半还没拉过。留空 = 让本地 agent 用供应商定义里
              // 的默认那个，比写一个我们还不知道存不存在的名字强
              model: '',
            ),
          );
    } on Object catch (e) {
      // **不打断保存。** 服务端那份已经存好了，在线完全可用 ——
      // 离线那份存不上只是少一个能力，不该让用户以为整个操作失败了
      if (mounted) {
        setState(() => _error = '来源已保存，但没能在本机留一份（离线时用不了）：$e');
      }
    }
  }

  Future<void> _edit(ModelSource s) async {
    final data = ref.read(modelSourcesProvider).value;
    if (data == null) return;
    final got = await showDialog<_SourceForm>(
      context: context,
      builder: (_) => _SourceDialog(providers: data.providers, initial: s),
    );
    if (got == null) return;
    await _run(
      () => ref
          .read(cortexApiProvider)
          .saveModelSource(
            id: s.id,
            provider: got.provider,
            apiKey: got.apiKey,
            label: got.label,
            baseUrl: got.baseUrl,
          ),
    );
    await _mirrorOffline(got);
  }

  Future<void> _delete(ModelSource s) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('删掉这条来源？'),
        content: const Text('里面那把 key 会一起删掉，不能撤销。只是想暂时不用的话，把它关掉即可。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(false),
            child: const Text('算了'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(ctx).pop(true),
            style: FilledButton.styleFrom(
              backgroundColor: Theme.of(ctx).colorScheme.error,
            ),
            child: const Text('删掉'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    await _run(() => ref.read(cortexApiProvider).deleteModelSource(s.id));

    // ⚠️ **本机那份也要清。**
    //
    // 不清的话，一把用户以为已经删掉的密钥会一直留在系统凭据库里 ——
    // 界面上那条来源没了，而离线时 agent 仍然拿着它去调。
    // 「我删了它」与「它还在用」同时成立，是删除操作最不该有的结果。
    //
    // 只清匹配这一家的：他可能有两条来源，删了 A 不该让 B 的离线也失效
    final local = ref.read(localLlmProvider).value;
    if (kCanStoreLocalLlm && local != null && local.provider == s.provider) {
      try {
        await ref.read(localLlmProvider.notifier).clear();
      } on Object catch (e) {
        if (mounted) {
          setState(() => _error = '来源已删除，但本机那份密钥没清掉：$e');
        }
      }
    }
  }

  Future<void> _toggle(ModelSource s, bool on) => _run(
    () => ref
        .read(cortexApiProvider)
        .saveModelSource(
          id: s.id,
          provider: s.provider,
          label: s.label,
          baseUrl: s.baseUrl,
          enabled: on,
        ),
  );

  Future<void> _saveModels(ModelSource s, List<String> models) => _run(
    () => ref
        .read(cortexApiProvider)
        .saveModelSource(
          id: s.id,
          provider: s.provider,
          label: s.label,
          baseUrl: s.baseUrl,
          models: models,
        ),
  );

  /// 去问供应商它有哪些型号，把结果存回这条来源。
  Future<void> _fetch(ModelSource s) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final got = await ref.read(cortexApiProvider).fetchSourceModels(s.id);
      if (!mounted) return;
      // 部署那条只读，拉回来只是给人看，不写库
      if (!s.builtin) {
        await ref
            .read(cortexApiProvider)
            .saveModelSource(
              id: s.id,
              provider: s.provider,
              label: s.label,
              baseUrl: s.baseUrl,
              models: got.models,
            );
        ref.invalidate(modelSourcesProvider);
        ref.invalidate(modelCatalogProvider);
      }
      if (!mounted) return;
      // ⚠️ **回落必须说出来。** 悄悄回落的表现是「我点了获取列表，
      // 它给了我一份看起来像样的、但其实是编译期写死的清单」，
      // 而那份清单可能与这个账号真正开通的东西毫无关系
      final msg = got.live
          ? '拉到 ${got.models.length} 个型号'
          : (got.note ?? '没拉到，用的是内置那份');
      ScaffoldMessenger.maybeOf(context)?.showSnackBar(
        SnackBar(content: Text(msg), duration: const Duration(seconds: 6)),
      );
    } on Object catch (e) {
      if (mounted) {
        setState(() => _error = e is CortexApiException ? e.message : e);
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }
}

/// 左边那一列。
class _SourceList extends StatelessWidget {
  const _SourceList({
    required this.data,
    required this.selected,
    required this.busy,
    required this.onSelect,
    required this.onAdd,
    required this.onToggle,
  });

  final ModelSources data;
  final String? selected;
  final bool busy;
  final ValueChanged<String> onSelect;
  final VoidCallback onAdd;
  final void Function(ModelSource, bool) onToggle;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Expanded(
          child: ListView(
            children: [
              for (final s in data.sources)
                _row(context, s, s.id == selected, theme),
            ],
          ),
        ),
        const SizedBox(height: 6),
        if (data.canAdd)
          OutlinedButton.icon(
            onPressed: busy ? null : onAdd,
            icon: const Icon(Icons.add, size: 16),
            label: const Text('添加模型'),
          )
        else
          // **如实说** —— 给一个存不进去的表单，用户会填、会点保存、
          // 会以为成了，下次打开又是空的
          Text(
            '这个部署存不了密钥（服务端未配 CORTEX_SECRET_KEY），只能用它自己配的那条。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
      ],
    );
  }

  Widget _row(BuildContext ctx, ModelSource s, bool on, ThemeData theme) {
    final scheme = theme.colorScheme;
    return InkWell(
      onTap: () => onSelect(s.id),
      borderRadius: BorderRadius.circular(8),
      child: Container(
        margin: const EdgeInsets.only(bottom: 4),
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 7),
        decoration: BoxDecoration(
          color: on ? scheme.primary.withValues(alpha: 0.10) : null,
          borderRadius: BorderRadius.circular(8),
        ),
        child: Row(
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    data.nameOf(s),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodyMedium?.copyWith(
                      color: s.enabled ? null : scheme.onSurfaceVariant,
                    ),
                  ),
                  Text(
                    _sub(s),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                ],
              ),
            ),
            // 开关而不是删除：删掉再填一遍 key 是最烦的一种「临时关掉」
            Switch(
              value: s.enabled,
              onChanged: busy ? null : (v) => onToggle(s, v),
            ),
          ],
        ),
      ),
    );
  }

  /// 一条来源的副标题：**先说钱，再说型号**。
  ///
  /// 「免费」是这一列里最要紧的一位 —— 它回答「用这条会不会花我的额度」。
  static String _sub(ModelSource s) {
    final bits = <String>[
      if (s.builtin) '免费 · 占配额' else '你的 key …${s.keyTail}',
      '${s.models.length} 个型号',
    ];
    return bits.join(' · ');
  }
}

/// 右边那一块：选中来源的详情。
class _Detail extends StatelessWidget {
  const _Detail({
    super.key,
    required this.data,
    required this.source,
    required this.busy,
    required this.onEdit,
    required this.onDelete,
    required this.onFetch,
    required this.onModels,
    this.offlineNote,
  });

  final ModelSources data;
  final ModelSource source;
  final bool busy;
  final VoidCallback onEdit;
  final VoidCallback onDelete;
  final VoidCallback onFetch;
  final ValueChanged<List<String>> onModels;

  /// 这条在离线那一侧是什么状态。`null` = 不用说（Web，或部署那条）。
  final String? offlineNote;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final p = data.providerOf(source.provider);
    final endpoint = source.baseUrl ?? p?.baseUrl ?? '';

    return ListView(
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                data.nameOf(source),
                style: theme.textTheme.titleSmall,
              ),
            ),
            if (!source.builtin) ...[
              TextButton(
                onPressed: busy ? null : onEdit,
                child: const Text('编辑'),
              ),
              TextButton(
                onPressed: busy ? null : onDelete,
                child: Text('删除', style: TextStyle(color: scheme.error)),
              ),
            ],
          ],
        ),
        if (source.builtin)
          Text(
            '这条是服务端配的，改不了也删不掉。它花的是我们的钱，所以计入你的配额。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          )
        else ...[
          Text(
            '密钥 …${source.keyTail} · 端点 $endpoint\n用它的调用走你自己的账户，不占配额。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
          // 离线那一侧的实况。**不说的话，用户会在断网时才发现** ——
          // 而那时他既不知道原因也不知道怎么办
          if (offlineNote != null)
            Padding(
              padding: const EdgeInsets.only(top: 6),
              child: Text(
                offlineNote!,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ),
        ],
        const SizedBox(height: 14),
        Row(
          children: [
            Expanded(
              child: Text(
                '模型（${source.models.length}）',
                style: theme.textTheme.labelLarge,
              ),
            ),
            TextButton.icon(
              onPressed: busy ? null : onFetch,
              icon: const Icon(Icons.refresh, size: 15),
              label: const Text('获取模型列表'),
            ),
          ],
        ),
        if (source.models.isEmpty)
          Text(
            // 不拿内置目录顶替：目录知道这家有几十个型号，但这个账号未必
            // 都开通了。填进选择器的每一个都必须是真的调得通的
            '还没有型号。点「获取模型列表」去问这家它到底开放了哪些 —— '
            '内置那份是编译期写死的，未必与你的账号一致。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          )
        else
          for (final m in source.models)
            ListTile(
              dense: true,
              contentPadding: EdgeInsets.zero,
              leading: const Icon(Icons.memory_outlined, size: 18),
              title: Text(m, style: theme.textTheme.bodySmall),
              trailing: source.builtin
                  ? null
                  : IconButton(
                      tooltip: '从这条来源移除',
                      iconSize: 16,
                      onPressed: busy
                          ? null
                          : () => onModels(
                              source.models.where((x) => x != m).toList(),
                            ),
                      icon: const Icon(Icons.close),
                    ),
            ),
      ],
    );
  }
}

/// 添加 / 编辑对话框的结果。
class _SourceForm {
  const _SourceForm({
    required this.provider,
    required this.apiKey,
    required this.label,
    required this.baseUrl,
  });
  final String provider;
  final String apiKey;
  final String label;
  final String baseUrl;
}

/// 添加或编辑一条来源。
///
/// # 供应商是下拉，不是输入框
///
/// 服务端只认它自己那份清单里的名字（保存时逐字校验）。让人手打的话，
/// 填对完全靠他猜中我们内部的 id —— `claude` 不行要写 `anthropic`、
/// `kimi` 不行要写 `moonshot`（实测被拒）。猜错的反馈来得也晚：
/// 填完、点保存，才收到一句「不认识的供应商」。
class _SourceDialog extends StatefulWidget {
  const _SourceDialog({required this.providers, this.initial});

  final List<ProviderChoice> providers;
  final ModelSource? initial;

  @override
  State<_SourceDialog> createState() => _SourceDialogState();
}

class _SourceDialogState extends State<_SourceDialog> {
  late String? _picked = _initialPick();
  late final _label = TextEditingController(text: widget.initial?.label ?? '');
  late final _baseUrl = TextEditingController(
    text: widget.initial?.baseUrl ?? '',
  );
  final _key = TextEditingController();
  bool _reveal = false;

  String? _initialPick() {
    if (widget.providers.isEmpty) return null;
    final want = widget.initial?.provider;
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
  void dispose() {
    _label.dispose();
    _baseUrl.dispose();
    _key.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final cur = _current;
    // 免鉴权的（本机 ollama）不该逼人填 key
    final needsKey = cur?.requiresAuth ?? true;
    final editing = widget.initial != null;

    return AlertDialog(
      title: Text(editing ? '编辑来源' : '添加模型'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
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
            TextField(
              controller: _label,
              autocorrect: false,
              decoration: const InputDecoration(
                labelText: '名字（可选）',
                hintText: '同一家配两条时用来分辨',
                border: OutlineInputBorder(),
              ),
            ),
            const SizedBox(height: 14),
            if (needsKey)
              TextField(
                controller: _key,
                autofocus: !editing,
                autocorrect: false,
                enableSuggestions: false,
                // 默认遮住、可点开：这是一串粘进来的东西，而一次看不见的
                // 粘贴迟早会错一次，且症状要到下一次对话才出现
                obscureText: !_reveal,
                decoration: InputDecoration(
                  labelText: 'API key',
                  // 改一条时留空 = 不动原来那把。界面永远拿不到明文，
                  // 所以「只想改个端点」根本没有 key 可以回填
                  hintText: editing ? '留空 = 不改动已存的那把' : null,
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
                // 说得出留空会连到哪 ——「用官方的」回答不了「官方的是哪个」，
                // 而那正是想改端点的人要核对的东西
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
          onPressed: () => Navigator.of(context).pop(
            _SourceForm(
              provider: _picked ?? '',
              apiKey: _key.text.trim(),
              label: _label.text.trim(),
              baseUrl: _baseUrl.text.trim(),
            ),
          ),
          child: const Text('保存'),
        ),
      ],
    );
  }
}
