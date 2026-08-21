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
/// # 2026-08-21：照 LobeHub 重做
///
/// 形态与信息密度对着 LobeHub 的「模型服务商」页做，三处结构变化：
///
/// 1. **「全部」总览**（卡片墙）—— 一屏看清有哪几家、开了哪几家；
/// 2. **来源列表按 已启用 / 未启用 分组**；
/// 3. **型号在列表里就地开关**，不再「拉完弹一个对话框勾选」。
///    第 3 条是服务端那个 `catalog` 字段解锁的：在此之前没勾中的型号
///    不在任何地方，想找回来只能重新拉一次。
///
/// 两处**有意不复刻**，理由都是「不画我们答不出的东西」：
///
/// * 型号的**发布日期** —— 目录里没有这一位；
/// * `视频 / 向量化 / ASR / TTS` 四个页签 —— 四类数据我们都没有，
///   画四个恒为 0 的页签，是在说一个不成立的能力。
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
import '../../../core/theme.dart';
import '../../../models/model_source.dart';
import '../../../state/app_providers.dart';
import '../../../state/model_controller.dart';
import '../widgets/model_table.dart';
import '../widgets/provider_mark.dart';
import '../widgets/provider_overview.dart';
import '../widgets/settings_layout.dart';

/// 左列里「全部」那一条的位置。`null` = 选中它。
///
/// 用可空而不是一个字符串常量：一个 `'__all__'` 之类的哨兵迟早会与
/// 某条来源的真实 id 相提并论（`byId('__all__')`），而那时错法是静默的。
typedef _Selection = String?;

class ModelPage extends ConsumerStatefulWidget {
  const ModelPage({super.key});

  @override
  ConsumerState<ModelPage> createState() => _ModelPageState();
}

class _ModelPageState extends ConsumerState<ModelPage> {
  /// 选中的那条来源。`null` = 「全部」总览。
  ///
  /// **默认落在总览上**：打开这一页最常见的意图是「我现在有什么」，
  /// 而不是「改第一条的 key」。
  _Selection _selected;

  String _query = '';
  bool _busy = false;
  Object? _error;

  /// 上一次连通性检查的结果。换一条来源就清掉 —— 一条属于 A 的结论
  /// 挂在 B 的详情页上，比没有结论更糟。
  SourceCheck? _check;

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
        final current = _selected == null ? null : data.byId(_selected!);
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
                    width: 200,
                    child: _SourceList(
                      data: data,
                      selected: _selected,
                      busy: _busy,
                      query: _query,
                      onQuery: (q) => setState(() => _query = q),
                      onSelect: _select,
                      onAdd: () => _add(data),
                      onToggle: _toggle,
                    ),
                  ),
                  const VerticalDivider(width: 20),
                  Expanded(
                    child: current == null
                        ? ProviderOverview(
                            data: data,
                            busy: _busy,
                            query: _query,
                            onOpen: _select,
                            onToggle: _toggle,
                            onAdd: (p) => _add(data, initial: p.id),
                          )
                        : _Detail(
                            key: ValueKey(current.id),
                            data: data,
                            source: current,
                            busy: _busy,
                            check: _check,
                            onSave: ({required apiKey, required baseUrl}) =>
                                _saveInline(current, apiKey, baseUrl),
                            onDelete: () => _delete(current),
                            onFetch: () => _fetch(current),
                            onToggleModel: (id, on) =>
                                _toggleModel(current, id, on),
                            onToggleSource: (on) => _toggle(current, on),
                            onCheck: (model) => _runCheck(current, model),
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

  void _select(_Selection id) => setState(() {
    _selected = id;
    // 换页就把上一条的检查结论丢掉
    _check = null;
  });

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

  Future<void> _add(ModelSources data, {String? initial}) async {
    final got = await showDialog<_SourceForm>(
      context: context,
      builder: (_) =>
          _SourceDialog(providers: data.providers, initial: initial),
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
  /// | 「本机没有这把 key」 | 在另一台设备上加的来源，这台机器离线用不了 —— 而界面上其余地方看起来它完全正常 |
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
  /// # 为什么不走弹窗
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
    // 删掉的那条不能继续选中着 —— 详情会指向一个不存在的 id
    if (mounted) _select(null);

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

  /// 开 / 关一个型号 —— **只动 `models`，不动 `catalog`**。
  ///
  /// 关掉的型号仍留在这条来源的全集里，所以它还在「未启用」组里看得见，
  /// 想再打开不用重新拉一次列表。这正是加 `catalog` 那个字段的全部理由。
  Future<void> _toggleModel(ModelSource s, String modelId, bool on) {
    // 部署那条只读：它的型号来自服务端的供应商定义，改不了
    if (s.builtin) return Future<void>.value();
    final next = on
        ? [...s.models, if (!s.models.contains(modelId)) modelId]
        : s.models.where((m) => m != modelId).toList();
    return _run(
      () => ref
          .read(cortexApiProvider)
          .saveModelSource(
            id: s.id,
            provider: s.provider,
            label: s.label,
            baseUrl: s.baseUrl,
            models: next,
          ),
    );
  }

  /// 去问供应商它有哪些型号。
  ///
  /// # 为什么拉完不再弹选择器
  ///
  /// 服务端现在会把拉到的全集存进 `catalog`，列表里就地开关 ——
  /// 弹窗那一步是多余的。而它原本存在的理由（那把 alibaba key 拉回来
  /// 240 个型号，整份塞进去会让撰写框的选择器变成 240 条）**照样成立**：
  /// 拉列表只动 `catalog`，`models` 一个字都不动，所以选择器里
  /// 仍然只有用户亲手打开的那几个。
  Future<void> _fetch(ModelSource s) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final got = await ref.read(cortexApiProvider).fetchSourceModels(s.id);
      // 回落时**必须说出来**：那份清单是编译期写死的，可能与这个账号
      // 真正开通的东西毫无关系
      if (mounted && !got.live && got.note != null) {
        setState(() => _error = got.note);
      }
      ref.invalidate(modelSourcesProvider);
    } on Object catch (e) {
      if (mounted) {
        setState(() => _error = e is CortexApiException ? e.message : e);
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _runCheck(ModelSource s, String model) async {
    setState(() {
      _busy = true;
      _check = null;
    });
    try {
      final got = await ref
          .read(cortexApiProvider)
          .checkModelSource(s.id, model: model);
      if (mounted) setState(() => _check = got);
    } on CortexApiException catch (e) {
      // 端点本身不存在（老服务端）与「检查没通过」是两回事，
      // 分开说 —— 混起来的话用户会去改一把其实没问题的 key
      if (mounted) {
        setState(
          () => _check = SourceCheck(
            ok: false,
            detail: e.isUnsupported ? '这个部署还没有连通性检查（服务端版本较早）' : e.message,
          ),
        );
      }
    } on Object catch (e) {
      if (mounted) {
        setState(() => _check = SourceCheck(ok: false, detail: '$e'));
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }
}

/// 左边那一列：「全部」+ 按启用状态分的两组。
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
  final _Selection selected;
  final bool busy;

  /// 搜索框里的字。空 = 全都显示。
  final String query;
  final ValueChanged<String> onQuery;
  final ValueChanged<_Selection> onSelect;
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
    final on = shown.where((s) => s.enabled).toList();
    final off = shown.where((s) => !s.enabled).toList();

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // 配了七八条来源之后，这一列就得翻。搜一下比翻快
        TextField(
          onChanged: onQuery,
          autocorrect: false,
          style: theme.textTheme.bodySmall,
          decoration: const InputDecoration(
            isDense: true,
            hintText: '搜索服务商',
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
                      color: theme.cortex.foregroundTertiary,
                    ),
                  ),
                )
              : ListView(
                  children: [
                    // 「全部」置顶：它是这一列里唯一一个**不指向某一条**的
                    // 入口，也是「我现在有什么」的答案
                    _AllRow(
                      key: const ValueKey('src:all'),
                      selected: selected == null,
                      onTap: () => onSelect(null),
                    ),
                    if (on.isNotEmpty) ...[
                      _group(theme, '已启用', on.length),
                      for (final s in on) _row(context, s, theme),
                    ],
                    if (off.isNotEmpty) ...[
                      _group(theme, '未启用', off.length),
                      for (final s in off) _row(context, s, theme),
                    ],
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
              color: theme.cortex.foregroundTertiary,
            ),
          ),
      ],
    );
  }

  Widget _group(ThemeData theme, String label, int count) => Padding(
    // 上 14 下 2：分组头属于**下面**那一组（规范第九节）
    padding: const EdgeInsets.fromLTRB(8, 14, 8, 2),
    child: Text(
      '$label $count',
      style: theme.textTheme.labelSmall?.copyWith(
        fontWeight: FontWeight.w600,
        color: theme.cortex.foregroundTertiary,
      ),
    ),
  );

  Widget _row(BuildContext ctx, ModelSource s, ThemeData theme) {
    final scheme = theme.colorScheme;
    final on = s.id == selected;
    return InkWell(
      // 稳定的 key。**同一条来源的名字在这一屏上出现两次**（左列一次、
      // 总览卡片一次），裸 `find.text` 指不准是哪一个 —— 而「点左列那一行」
      // 与「点那张卡」是两个不同的动作
      key: ValueKey('src:${s.id}'),
      onTap: () => onSelect(s.id),
      borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
      child: Container(
        margin: const EdgeInsets.only(bottom: 2),
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
        decoration: BoxDecoration(
          // **选中态是中性的**（规范第九节）。「我现在在看哪一条」
          // 是位置，不是动作
          color: on ? theme.cortex.sidebarAccent : null,
          borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
        ),
        child: Row(
          children: [
            ProviderMark(
              provider: s.provider,
              displayName: data.nameOf(s),
              size: 18,
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                data.nameOf(s),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.bodySmall?.copyWith(
                  // 关掉的那条整体退到第三级 —— 它还在，但不参与对话
                  color: s.enabled
                      ? scheme.onSurface
                      : theme.cortex.foregroundTertiary,
                ),
              ),
            ),
            // 开着的画一个小圆点，而不是再摆一个开关：这一列已经按
            // 已启用/未启用 分了组，开关在这里是同一件事说第三遍，
            // 而它还会把行高撑起来
            if (s.enabled)
              Container(
                width: 6,
                height: 6,
                // 圆点，不走圆角那五阶
                decoration: BoxDecoration(
                  color: theme.cortex.success,
                  shape: BoxShape.circle,
                ),
              ),
          ],
        ),
      ),
    );
  }
}

/// 「全部」那一条。
class _AllRow extends StatelessWidget {
  const _AllRow({super.key, required this.selected, required this.onTap});

  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 7),
        decoration: BoxDecoration(
          color: selected ? theme.cortex.sidebarAccent : null,
          borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
        ),
        child: Row(
          children: [
            Icon(
              Icons.apps_rounded,
              size: 18,
              color: theme.colorScheme.onSurfaceVariant,
            ),
            const SizedBox(width: 8),
            Text(
              '全部',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurface,
                fontWeight: selected ? FontWeight.w600 : null,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// 右边那一块：选中来源的详情。
///
/// # 形态照 LobeHub 的「模型服务商」
///
/// **密钥与端点直接在这里改，不弹窗。** 弹窗那版每次想核对一眼端点都要
/// 点开、关掉，而端点恰恰是最常需要核对的一项（国内站 / 国际站、公司网关）
/// —— 那把 alibaba key 的 401 就是端点错了，而弹窗把它藏了起来。
class _Detail extends StatefulWidget {
  const _Detail({
    super.key,
    required this.data,
    required this.source,
    required this.busy,
    required this.check,
    required this.onSave,
    required this.onDelete,
    required this.onFetch,
    required this.onToggleModel,
    required this.onToggleSource,
    required this.onCheck,
    required this.offlineNote,
  });

  final ModelSources data;
  final ModelSource source;
  final bool busy;
  final SourceCheck? check;
  final void Function({required String apiKey, required String baseUrl}) onSave;
  final VoidCallback onDelete;
  final VoidCallback onFetch;
  final void Function(String, bool) onToggleModel;
  final ValueChanged<bool> onToggleSource;
  final ValueChanged<String> onCheck;
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

  /// 拿哪个型号去验。空 = 让服务端挑第一个开着的。
  String _probe = '';

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
        _header(context, s),
        if (s.builtin)
          const Padding(
            padding: EdgeInsets.only(top: 10),
            child: SettingsNote(
              icon: Icons.lock_outline_rounded,
              child: Text('这条是服务端配的，改不了也删不掉。它花的是我们的钱，所以计入你的配额。'),
            ),
          )
        else ...[
          SettingsField(
            label: 'API 密钥',
            hint: '留空 = 不改动原来那把',
            child: TextField(
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
                // 界面永远拿不到明文，所以只能显示后四位当占位
                hintText: '已存 …${s.keyTail}',
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
          ),
          SettingsField(
            label: 'API 代理地址',
            // 说得出留空会连到哪 ——「用官方的」回答不了「官方的是哪个」，
            // 而那正是想改端点的人要核对的东西
            hint: official.isEmpty ? '必须包含 http(s)://' : '留空 = $official',
            child: TextField(
              // 有名字才指得准。这一页上现在有四个输入框（来源搜索、密钥、
              // 端点、型号搜索），按位置找（`byType(TextField).last`）
              // 会随着页面加一个框就悄悄指到别处 —— 2026-08-21 就是这么
              // 让一条离线镜像的测试红的，而它测的东西一点没变
              key: const ValueKey('field:base-url'),
              controller: _baseUrl,
              autocorrect: false,
              enableSuggestions: false,
              onChanged: (_) => setState(() => _dirty = true),
              decoration: const InputDecoration(
                isDense: true,
                border: OutlineInputBorder(),
                hintText: 'https://…',
              ),
            ),
          ),
          if (_dirty)
            Align(
              alignment: Alignment.centerRight,
              child: FilledButton(
                onPressed: widget.busy ? null : _save,
                child: const Text('保存'),
              ),
            ),
          _checkField(context, s),
          const SizedBox(height: 10),
          // **这句是真的**：`ring::aead::AES_256_GCM`，密钥与端点一起封装，
          // 主密钥来自服务端的 CORTEX_SECRET_KEY
          Center(
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  Icons.lock_outline_rounded,
                  size: 13,
                  color: theme.cortex.foregroundTertiary,
                ),
                const SizedBox(width: 5),
                Text(
                  '你的密钥与代理地址在服务端以 AES-256-GCM 加密存储，界面永远只拿得到后四位',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: theme.cortex.foregroundTertiary,
                  ),
                ),
              ],
            ),
          ),
          const SizedBox(height: 6),
          if (widget.offlineNote != null)
            SettingsNote(
              child: Text(
                '用它的调用走你自己的账户，不占配额。\n${widget.offlineNote}',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ),
        ],
        const SizedBox(height: 24),
        ModelTable(
          source: s,
          busy: widget.busy,
          onToggle: widget.onToggleModel,
          onFetch: widget.onFetch,
        ),
      ],
    );
  }

  Widget _header(BuildContext context, ModelSource s) {
    final theme = Theme.of(context);
    return Row(
      children: [
        ProviderMark(
          provider: s.provider,
          displayName: widget.data.nameOf(s),
          size: 26,
        ),
        const SizedBox(width: 10),
        Expanded(
          child: Text(
            widget.data.nameOf(s),
            style: theme.textTheme.titleMedium,
          ),
        ),
        if (!s.builtin)
          TextButton(
            onPressed: widget.busy ? null : widget.onDelete,
            child: Text('删除', style: TextStyle(color: theme.colorScheme.error)),
          ),
        const SizedBox(width: 4),
        Switch(
          value: s.enabled,
          onChanged: widget.busy ? null : widget.onToggleSource,
        ),
      ],
    );
  }

  /// 连通性检查那一行。
  ///
  /// 型号下拉只列**开着的**：验一个自己没打开的型号，通过了也用不上，
  /// 而没通过又说明不了当前配置有问题。
  Widget _checkField(BuildContext context, ModelSource s) {
    final theme = Theme.of(context);
    final result = widget.check;
    final options = s.models;

    return SettingsField(
      label: '连通性检查',
      hint: '拿存下来的密钥真发一次请求，验它对不对',
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: DropdownButtonFormField<String>(
                  initialValue: options.contains(_probe) ? _probe : null,
                  isExpanded: true,
                  decoration: const InputDecoration(
                    isDense: true,
                    border: OutlineInputBorder(),
                    hintText: '挑一个已启用的型号',
                  ),
                  items: [
                    for (final m in options)
                      DropdownMenuItem(
                        value: m,
                        child: Text(m, style: theme.textTheme.bodySmall),
                      ),
                  ],
                  onChanged: (v) => setState(() => _probe = v ?? ''),
                ),
              ),
              const SizedBox(width: 8),
              OutlinedButton(
                // 一个型号都没开时按不动，并在下面说清为什么 ——
                // 一个点了没反应的按钮比没有这个按钮更让人困惑
                onPressed: widget.busy || options.isEmpty
                    ? null
                    : () => widget.onCheck(_probe),
                child: const Text('检查'),
              ),
            ],
          ),
          if (options.isEmpty)
            Padding(
              padding: const EdgeInsets.only(top: 6),
              child: Text(
                '先在下面打开至少一个型号，才有东西可以拿去验。',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                ),
              ),
            ),
          if (result != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: SettingsNote(
                icon: result.ok
                    ? Icons.check_circle_outline
                    : Icons.error_outline,
                // 成功/失败是**状态**，正是反馈色该用的地方（规范第五节）
                tone: result.ok
                    ? theme.cortex.success
                    : theme.colorScheme.error,
                child: Text(
                  result.detail,
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: result.ok
                        ? theme.cortex.success
                        : theme.colorScheme.error,
                  ),
                ),
              ),
            ),
        ],
      ),
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

/// 添加一条来源。
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

  /// 从卡片墙点进来时预选的那一家。**省掉一次「我刚点的是哪家来着」** ——
  /// 那面墙上有十几张卡，让人在下拉里再找一遍是纯粹的重复劳动。
  final String? initial;

  @override
  State<_SourceDialog> createState() => _SourceDialogState();
}

class _SourceDialogState extends State<_SourceDialog> {
  late String? _picked = _initialPick();
  final _label = TextEditingController();
  final _baseUrl = TextEditingController();
  final _key = TextEditingController();
  bool _reveal = false;

  String? _initialPick() {
    final want = widget.initial;
    if (want != null && widget.providers.any((p) => p.id == want)) return want;
    return widget.providers.isEmpty ? null : widget.providers.first.id;
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
    final needsKey = cur?.requiresAuth ?? true;

    return AlertDialog(
      title: const Text('添加模型来源'),
      content: SizedBox(
        width: 420,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              DropdownButtonFormField<String>(
                initialValue: _picked,
                isExpanded: true,
                decoration: const InputDecoration(
                  labelText: '供应商',
                  isDense: true,
                  border: OutlineInputBorder(),
                ),
                items: [
                  for (final p in widget.providers)
                    DropdownMenuItem(
                      value: p.id,
                      child: Row(
                        children: [
                          ProviderMark(
                            provider: p.id,
                            displayName: p.displayName,
                            size: 16,
                          ),
                          const SizedBox(width: 8),
                          Text(p.displayName),
                        ],
                      ),
                    ),
                ],
                onChanged: (v) => setState(() => _picked = v),
              ),
              if (cur != null && cur.description.isNotEmpty) ...[
                const SizedBox(height: 6),
                Text(cur.description, style: theme.textTheme.labelSmall),
              ],
              const SizedBox(height: 12),
              // ⚠️ **不需要 key 的那家整个不画这个框**，不是改个标签。
              //
              // 本机 ollama 没有密钥这回事。留着一个写「（这家不需要）」的
              // 输入框，用户仍然会盯着它想「那我是不是漏填了什么」——
              // 一个不该填的框，最省事的处理是让它不存在。
              if (needsKey)
                TextField(
                  controller: _key,
                  autocorrect: false,
                  enableSuggestions: false,
                  obscureText: !_reveal,
                  // ⚠️ **这一行不能省。** 下面那个「添加」按钮在 key 为空时
                  // 是禁用的，而禁用与否是在 `build` 里算的 —— 不随输入重建
                  // 的话，用户填完 key 那个按钮**仍然按不动**，
                  // 而界面上看不出任何理由
                  onChanged: (_) => setState(() {}),
                  decoration: InputDecoration(
                    labelText: 'API key',
                    isDense: true,
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
                  '这家不需要密钥。',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: theme.cortex.foregroundTertiary,
                  ),
                ),
              const SizedBox(height: 12),
              TextField(
                controller: _baseUrl,
                autocorrect: false,
                enableSuggestions: false,
                decoration: InputDecoration(
                  labelText: 'API 代理地址（可留空）',
                  isDense: true,
                  border: const OutlineInputBorder(),
                  hintText: (cur?.baseUrl.isNotEmpty ?? false)
                      ? '留空 = ${cur!.baseUrl}'
                      : 'https://…',
                ),
              ),
              const SizedBox(height: 12),
              TextField(
                controller: _label,
                decoration: const InputDecoration(
                  labelText: '备注名（可留空）',
                  isDense: true,
                  border: OutlineInputBorder(),
                  // 同一家配两条时全靠它分辨 —— 说清它是干嘛的
                  hintText: '同一家配两条时用来区分，如「公司网关」',
                ),
              ),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: _picked == null || (needsKey && _key.text.trim().isEmpty)
              ? null
              : () => Navigator.of(context).pop(
                  _SourceForm(
                    provider: _picked!,
                    apiKey: _key.text.trim(),
                    label: _label.text.trim(),
                    baseUrl: _baseUrl.text.trim(),
                  ),
                ),
          child: const Text('添加'),
        ),
      ],
    );
  }
}
