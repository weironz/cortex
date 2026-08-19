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
import '../../../models/model_option.dart';
import '../../../models/model_source.dart';
import '../../../state/app_providers.dart';
import '../../../state/model_controller.dart';
import 'model_add_dialog.dart';

class ModelPage extends ConsumerStatefulWidget {
  const ModelPage({super.key});

  @override
  ConsumerState<ModelPage> createState() => _ModelPageState();
}

class _ModelPageState extends ConsumerState<ModelPage> {
  /// 选中的那条。`null` = 还没选，显示第一条。
  String? _selected;
  String _query = '';
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
                      query: _query,
                      onQuery: (q) => setState(() => _query = q),
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
                            onSave: ({required apiKey, required baseUrl}) =>
                                _saveInline(current, apiKey, baseUrl),
                            onDelete: () => _delete(current),
                            onFetch: () => _fetch(current),
                            onModels: (m) => _saveModels(current, m),
                            known: _known(),
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

  /// 型号名 → 能力，取自已经拉过的那份目录（`/llm/models`）。
  ///
  /// **不为几个图标再发一次请求**：那份目录本来就要拉（撰写框的选择器
  /// 用它），这里只是复用。查不到的型号不出现在这个表里，界面据此不画徽标。
  Map<String, ModelOption> _known() {
    final c = ref.watch(modelCatalogProvider).value;
    if (c == null) return const {};
    return {for (final m in c.models) m.id: m};
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
        '想让它能离线用：在上面的「API 密钥」里重填一遍。';
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

  /// 内联保存密钥 / 端点。
  ///
  /// # 为什么不再走弹窗
  ///
  /// 弹窗版每次想核对一眼端点都要点开、关掉。而端点恰恰是最常需要核对的
  /// 一项 —— 那把 alibaba key 的 401 就是端点错了（内置默认国际站），
  /// 而弹窗把它藏在了两次点击之后。
  Future<void> _saveInline(ModelSource s, String apiKey, String baseUrl) async {
    await _run(
      () => ref
          .read(cortexApiProvider)
          .saveModelSource(
            id: s.id,
            provider: s.provider,
            apiKey: apiKey,
            label: s.label,
            baseUrl: baseUrl,
          ),
    );
    await _mirrorOffline(
      _SourceForm(
        provider: s.provider,
        apiKey: apiKey,
        label: s.label,
        baseUrl: baseUrl,
      ),
    );
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

  /// 去问供应商它有哪些型号，然后**让用户挑**。
  ///
  /// # 为什么不再整份塞进来
  ///
  /// 那把 alibaba key 拉回来 240 个型号。整份塞进去之后，撰写框的选择器里
  /// 就是 240 条，而真正会用的不超过五个。
  Future<void> _fetch(ModelSource s) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    FetchedModels? got;
    try {
      got = await ref.read(cortexApiProvider).fetchSourceModels(s.id);
    } on Object catch (e) {
      if (mounted) {
        setState(() => _error = e is CortexApiException ? e.message : e);
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
    if (got == null || !mounted) return;

    final picked = await showAddModels(
      context,
      fetched: got,
      already: s.models,
    );
    if (picked == null || picked.isEmpty || !mounted) return;

    // 部署那条只读 —— 拉回来只是给人看
    if (s.builtin) return;
    await _saveModels(s, [...s.models, ...picked]);
  }
}

/// 左边那一列。
class _SourceList extends StatelessWidget {
  const _SourceList({
    required this.data,
    required this.selected,
    required this.busy,
    required this.query,
    required this.onQuery,
    required this.onSelect,
    required this.onAdd,
    required this.onToggle,
  });

  final ModelSources data;
  final String? selected;
  final bool busy;

  /// 搜索框里的字。空 = 全都显示。
  final String query;
  final ValueChanged<String> onQuery;
  final ValueChanged<String> onSelect;
  final VoidCallback onAdd;
  final void Function(ModelSource, bool) onToggle;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final keep = query.trim().toLowerCase();
    final shown = keep.isEmpty
        ? data.sources
        : data.sources
              .where(
                (s) =>
                    data.nameOf(s).toLowerCase().contains(keep) ||
                    s.provider.toLowerCase().contains(keep),
              )
              .toList();

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // 配了七八条来源之后，这一列就得翻。搜一下比翻快
        TextField(
          onChanged: onQuery,
          autocorrect: false,
          decoration: const InputDecoration(
            isDense: true,
            hintText: '搜索',
            prefixIcon: Icon(Icons.search, size: 16),
            border: OutlineInputBorder(),
          ),
        ),
        const SizedBox(height: 8),
        Expanded(
          child: shown.isEmpty
              ? Center(
                  child: Text(
                    '没有匹配的来源',
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                  ),
                )
              : ListView(
                  children: [
                    for (final s in shown)
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
/// 右边那一块：选中来源的详情。
///
/// # 形态照 Cherry Studio 的「模型服务」
///
/// **密钥与端点直接在这里改，不弹窗。** 弹窗那版每次想核对一眼端点都要
/// 点开、关掉，而端点恰恰是最常需要核对的一项（国内站 / 国际站、公司网关）
/// —— 那把 alibaba key 的 401 就是端点错了，而弹窗把它藏了起来。
///
/// 型号**按系列分组**（`qwen-image-3.0` → `qwen`）。240 个型号铺成一条
/// 平列表没法看，而同一系列的东西本来就该待在一起。
class _Detail extends StatefulWidget {
  const _Detail({
    super.key,
    required this.data,
    required this.source,
    required this.busy,
    required this.onSave,
    required this.onDelete,
    required this.onFetch,
    required this.onModels,
    required this.known,
    this.offlineNote,
  });

  final ModelSources data;
  final ModelSource source;
  final bool busy;

  /// 存改动。`apiKey` 空串 = 不动原来那把。
  final void Function({required String apiKey, required String baseUrl}) onSave;
  final VoidCallback onDelete;
  final VoidCallback onFetch;
  final ValueChanged<List<String>> onModels;

  /// 型号名 → 它的能力（来自已经拉过的那份目录）。查不到的就不画徽标。
  final Map<String, ModelOption> known;

  /// 这条在离线那一侧是什么状态。`null` = 不用说（Web，或部署那条）。
  final String? offlineNote;

  @override
  State<_Detail> createState() => _DetailState();
}

class _DetailState extends State<_Detail> {
  final _key = TextEditingController();
  late final _baseUrl = TextEditingController(
    text: widget.source.baseUrl ?? '',
  );
  bool _reveal = false;
  bool _dirty = false;

  @override
  void dispose() {
    _key.dispose();
    _baseUrl.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final s = widget.source;
    final official = widget.data.providerOf(s.provider)?.baseUrl ?? '';

    return ListView(
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                widget.data.nameOf(s),
                style: theme.textTheme.titleSmall,
              ),
            ),
            if (!s.builtin)
              TextButton(
                onPressed: widget.busy ? null : widget.onDelete,
                child: Text('删除', style: TextStyle(color: scheme.error)),
              ),
          ],
        ),
        if (s.builtin)
          Text(
            '这条是服务端配的，改不了也删不掉。它花的是我们的钱，所以计入你的配额。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          )
        else ...[
          const SizedBox(height: 12),
          Text('API 密钥', style: theme.textTheme.labelLarge),
          const SizedBox(height: 4),
          TextField(
            controller: _key,
            autocorrect: false,
            enableSuggestions: false,
            // 默认遮住、可点开：这是一串粘进来的东西，而一次看不见的粘贴
            // 迟早会错一次，且症状要到下一次对话才出现
            obscureText: !_reveal,
            onChanged: (_) => setState(() => _dirty = true),
            decoration: InputDecoration(
              isDense: true,
              border: const OutlineInputBorder(),
              // 界面永远拿不到明文，所以只能显示后四位当占位。
              // 留空保存 = 不动原来那把
              hintText: '已存 …${s.keyTail}，留空 = 不改动',
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
          const SizedBox(height: 12),
          Text('API 地址', style: theme.textTheme.labelLarge),
          const SizedBox(height: 4),
          TextField(
            controller: _baseUrl,
            autocorrect: false,
            enableSuggestions: false,
            onChanged: (_) => setState(() => _dirty = true),
            decoration: InputDecoration(
              isDense: true,
              border: const OutlineInputBorder(),
              // 说得出留空会连到哪 ——「用官方的」回答不了「官方的是哪个」，
              // 而那正是想改端点的人要核对的东西
              hintText: official.isEmpty ? '留空 = 用官方的' : '留空 = $official',
            ),
          ),
          if (_dirty)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Align(
                alignment: Alignment.centerRight,
                child: FilledButton(
                  onPressed: widget.busy ? null : _save,
                  child: const Text('保存'),
                ),
              ),
            ),
          const SizedBox(height: 6),
          Text(
            '用它的调用走你自己的账户，不占配额。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
          // 离线那一侧的实况。**不说的话，用户会在断网时才发现** ——
          // 而那时他既不知道原因也不知道怎么办
          if (widget.offlineNote != null)
            Padding(
              padding: const EdgeInsets.only(top: 6),
              child: Text(
                widget.offlineNote!,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ),
        ],
        const SizedBox(height: 16),
        Row(
          children: [
            Expanded(
              child: Text(
                '模型（${s.models.length}）',
                style: theme.textTheme.labelLarge,
              ),
            ),
            TextButton.icon(
              onPressed: widget.busy ? null : widget.onFetch,
              icon: const Icon(Icons.refresh, size: 15),
              label: const Text('获取模型列表'),
            ),
          ],
        ),
        if (s.models.isEmpty)
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
          for (final group in groupModels(s.models)) ...[
            Padding(
              padding: const EdgeInsets.fromLTRB(0, 10, 0, 2),
              child: Text(
                '${group.$1}（${group.$2.length}）',
                style: theme.textTheme.labelSmall?.copyWith(
                  fontWeight: FontWeight.w700,
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ),
            for (final m in group.$2)
              _ModelRow(
                model: m,
                // 能力从**已经拉过的那份目录**里查，不为几个图标再发一次
                // 请求。查不到就不画徽标 —— 「不知道」不该长得像「不支持」
                known: widget.known[m],
                busy: widget.busy,
                onRemove: s.builtin
                    ? null
                    : () => widget.onModels(
                        s.models.where((x) => x != m).toList(),
                      ),
              ),
          ],
      ],
    );
  }

  void _save() {
    widget.onSave(apiKey: _key.text.trim(), baseUrl: _baseUrl.text.trim());
    setState(() {
      _dirty = false;
      _key.clear();
    });
  }
}

/// 型号列表里的一行。
class _ModelRow extends StatelessWidget {
  const _ModelRow({
    required this.model,
    required this.known,
    required this.busy,
    required this.onRemove,
  });

  final String model;

  /// 它的能力。`null` = 目录里查不到 —— **不画徽标**，因为一个灰徽标
  /// 会被读成「不支持」，而实际是「不知道」。
  final ModelOption? known;

  final bool busy;
  final VoidCallback? onRemove;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 1),
      child: Row(
        children: [
          Expanded(
            child: Text(
              model,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodySmall,
            ),
          ),
          for (final b in badgesOfOption(known))
            Padding(
              padding: const EdgeInsets.only(left: 6),
              child: Tooltip(
                message: b.$2,
                child: Icon(
                  b.$1,
                  size: 13,
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
            ),
          if (onRemove != null)
            IconButton(
              tooltip: '从这条来源移除',
              iconSize: 14,
              visualDensity: VisualDensity.compact,
              onPressed: busy ? null : onRemove,
              icon: const Icon(Icons.close),
            ),
        ],
      ),
    );
  }
}

/// 按**系列**把型号分组：`qwen-image-3.0` 与 `qwen-turbo` 都进 `qwen`。
///
/// 240 个型号铺成一条平列表没法看，而同一系列的东西本来就该待在一起。
/// 组内与组间都保持服务端给的顺序 —— 那是它 `/models` 的顺序，
/// 重排一次就多一个「我记得它在上面」的困惑。
List<(String, List<String>)> groupModels(List<String> models) {
  final out = <(String, List<String>)>[];
  for (final m in models) {
    final i = m.indexOf('-');
    final family = i <= 0 ? m : m.substring(0, i);
    final at = out.indexWhere((g) => g.$1 == family);
    if (at < 0) {
      out.add((family, <String>[m]));
    } else {
      out[at].$2.add(m);
    }
  }
  return out;
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
  const _SourceDialog({required this.providers});

  final List<ProviderChoice> providers;

  @override
  State<_SourceDialog> createState() => _SourceDialogState();
}

class _SourceDialogState extends State<_SourceDialog> {
  late String? _picked = _initialPick();
  final _label = TextEditingController();
  final _baseUrl = TextEditingController();
  final _key = TextEditingController();
  bool _reveal = false;

  String? _initialPick() =>
      widget.providers.isEmpty ? null : widget.providers.first.id;

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

    return AlertDialog(
      title: const Text('添加模型'),
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
                autofocus: true,
                autocorrect: false,
                enableSuggestions: false,
                // 默认遮住、可点开：这是一串粘进来的东西，而一次看不见的
                // 粘贴迟早会错一次，且症状要到下一次对话才出现
                obscureText: !_reveal,
                decoration: InputDecoration(
                  labelText: 'API key',
                  // 改一条时留空 = 不动原来那把。界面永远拿不到明文，
                  // 所以「只想改个端点」根本没有 key 可以回填
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
