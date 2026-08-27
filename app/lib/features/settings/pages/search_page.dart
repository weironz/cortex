/// 设置 → 联网检索。
///
/// # 为什么这一页值得存在
///
/// 在它之前，联网检索唯一的配置是服务端 `.env` 里的一把 key。于是：
/// 用户想换一家（或者用自己的额度）得去改服务器，而界面上**一个开关都
/// 没有** —— 他甚至不知道这个能力存不存在。
///
/// 形状照 Cherry Studio 的同名页：服务商下拉 + key + 地址 + 高级设置 +
/// 黑名单，外加一个独立的「URL 获取服务商」。抄的是**分区方式**，
/// 不是代码（它是 AGPL，我们不兼容）。
///
/// # ⚠️ 与那一页的两处有意不同
///
/// 1. **key 存在服务端，不在这台机器上。** Cherry 是本地应用，key 躺在本地
///    配置里；这里是客户端/服务端，而真正打上游的是服务端。所以这一页只
///    看得到后四位 —— 与「模型服务」那一页同一条约定。
/// 2. **多一档「部署提供」。** 服务端 `.env` 里那把仍然是回落，没配过的人
///    行为与从前一字不差。Cherry 没有这一档，因为它没有「部署方」这个角色。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/theme.dart';
import '../../../models/search_prefs.dart';
import '../../../state/app_providers.dart';
import '../../../widgets/panel_header.dart';

/// 这个部署的检索配置。
///
/// `autoDispose` + 每次进页面重拉：它跟着服务端走，而用户打开这一页正是
/// 想看**现在**是什么设置。
final searchPrefsProvider = FutureProvider.autoDispose<SearchPrefs>(
  (ref) => ref.watch(cortexApiProvider).searchPrefs(),
  retry: (count, error) {
    // 与模型目录同一个判据：没有这条路不是故障，重试永远不会成功
    if (error is CortexApiException && error.isUnsupported) return null;
    final secs = [1, 3, 8, 20, 30];
    return Duration(seconds: secs[count.clamp(0, secs.length - 1)]);
  },
);

class SearchPage extends ConsumerWidget {
  const SearchPage({super.key, this.onToggleSessions});

  final VoidCallback? onToggleSessions;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final prefs = ref.watch(searchPrefsProvider);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const PanelHeader(title: '联网检索', subtitle: '服务商 · key · 检索深度'),
        Expanded(
          child: prefs.when(
            skipLoadingOnReload: true,
            skipLoadingOnRefresh: true,
            loading: () => const Center(child: CircularProgressIndicator()),
            error: (e, _) => Center(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Text(
                  e is CortexApiException && e.isUnsupported
                      // 老服务端没有这条路。**说清是「这个部署没有」而不是
                      // 「出错了」** —— 后者会让人去重试、去重启
                      ? '这个部署的服务端还没有联网检索的配置接口（升级服务端之后就有了）。'
                      : '读不出联网检索的配置：$e',
                  textAlign: TextAlign.center,
                  style: theme.textTheme.bodyMedium?.copyWith(
                    color: theme.colorScheme.onSurfaceVariant,
                  ),
                ),
              ),
            ),
            data: (p) => _Form(prefs: p),
          ),
        ),
      ],
    );
  }
}

class _Form extends ConsumerStatefulWidget {
  const _Form({required this.prefs});

  final SearchPrefs prefs;

  @override
  ConsumerState<_Form> createState() => _FormState();
}

class _FormState extends ConsumerState<_Form> {
  late final TextEditingController _key;
  late final TextEditingController _base;
  late final TextEditingController _deny;
  late String _provider;
  late String _depth;
  late int _maxResults;
  late int _cutoff;
  bool _busy = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    final p = widget.prefs;
    // ⚠️ key 那个框**开着就是空的**：我们从来拿不到明文。它的占位符写着
    // 已存那把的后四位 —— 用户据此知道「已经填过了」，而空着提交不会
    // 清掉它（缺省 = 不动，见 `saveSearchPrefs`）
    _key = TextEditingController();
    _base = TextEditingController(text: p.baseUrl ?? '');
    _deny = TextEditingController(text: p.excludeDomains.join('\n'));
    _provider = p.provider;
    _depth = p.depth;
    _maxResults = p.maxResults;
    _cutoff = p.cutoffLimit;
  }

  @override
  void dispose() {
    _key.dispose();
    _base.dispose();
    _deny.dispose();
    super.dispose();
  }

  Future<void> _save({String? apiKey}) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await ref
          .read(cortexApiProvider)
          .saveSearchPrefs(
            provider: _provider,
            apiKey: apiKey,
            baseUrl: _base.text.trim(),
            maxResults: _maxResults,
            depth: _depth,
            cutoffLimit: _cutoff,
            excludeDomains: _deny.text
                .split('\n')
                .map((s) => s.trim())
                .where((s) => s.isNotEmpty)
                .toList(),
          );
      if (!mounted) return;
      _key.clear();
      ref.invalidate(searchPrefsProvider);
    } on CortexApiException catch (e) {
      // 服务端那句话原样显示：它写的是用户能照做的那一步
      // （「深度只有两档」「一次回几条只能在 1 到 20 之间」）
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final p = widget.prefs;
    final deployment = _provider.isEmpty;

    return ListView(
      padding: const EdgeInsets.fromLTRB(18, 12, 18, 24),
      children: [
        _StatusLine(prefs: p),
        const SizedBox(height: 16),

        _section(theme, '搜索服务商'),
        _row(
          theme,
          label: '服务商',
          hint: deployment ? '用这个部署配好的那一份 —— 不用填 key' : '这一家的 key 与地址在下面填',
          child: DropdownButtonFormField<String>(
            key: const ValueKey('search:provider'),
            initialValue: _provider,
            decoration: const InputDecoration(isDense: true),
            items: [
              // 「部署提供」永远排第一 —— 它是没配过的人的现状，
              // 把它藏在列表末尾等于让人以为自己必须选一家
              const DropdownMenuItem(value: '', child: Text('部署提供')),
              for (final x in p.providers)
                DropdownMenuItem(value: x.id, child: Text(x.name)),
            ],
            onChanged: _busy
                ? null
                : (v) => setState(() => _provider = v ?? ''),
          ),
        ),

        // 选了「部署提供」就不摆 key 与地址：那两个框在那一档下点了没用，
        // 而摆一个点了没用的框比不摆更糟（约束 2）
        if (!deployment) ...[
          _row(
            theme,
            label: 'API 密钥',
            hint: p.keyTail.isEmpty ? '还没填过' : '已存一把（…${p.keyTail}）。留空 = 不动它',
            child: TextField(
              controller: _key,
              obscureText: true,
              enabled: !_busy,
              decoration: InputDecoration(
                isDense: true,
                hintText: p.keyTail.isEmpty ? '粘贴 key' : '••••${p.keyTail}',
              ),
            ),
          ),
          _row(
            theme,
            label: 'API 地址',
            hint: '留空 = 用官方地址。中转站 / 自建服务填这里',
            child: TextField(
              controller: _base,
              enabled: !_busy,
              decoration: InputDecoration(
                isDense: true,
                // 同上：要的是**当下选的**那家的官方地址。用 `p.current`
                // 的话，刚换过下拉框时这里显示的还是上一家的地址
                hintText: p.byId(_provider)?.defaultBase ?? '',
              ),
            ),
          ),
        ],

        const SizedBox(height: 20),
        _section(theme, '高级设置'),
        _row(
          theme,
          label: '搜索结果个数',
          hint: '多了占上下文，少了模型看不到足够的角度',
          child: _Stepper(
            value: _maxResults,
            min: 1,
            max: 20,
            enabled: !_busy,
            onChanged: (v) => setState(() => _maxResults = v),
          ),
        ),
        _row(
          theme,
          label: '检索深度',
          // ⚠️ 这一位直接影响账单，所以说明里必须写出来 —— 一个「更好一点」
          // 的开关会让人在不知情的情况下多花一倍钱
          hint: 'advanced 结果更好，但多数服务商按两倍计费',
          child: SegmentedButton<String>(
            segments: const [
              ButtonSegment(value: 'basic', label: Text('basic')),
              ButtonSegment(value: 'advanced', label: Text('advanced')),
            ],
            selected: {_depth},
            showSelectedIcon: false,
            onSelectionChanged: _busy
                ? null
                : (v) => setState(() => _depth = v.first),
          ),
        ),
        _row(
          theme,
          label: '每条结果截到多长',
          hint: '0 = 不截。basic 档的摘要本来就短，这一位主要给 advanced 用',
          child: _Stepper(
            value: _cutoff,
            min: 0,
            max: 8000,
            step: 500,
            enabled: !_busy,
            onChanged: (v) => setState(() => _cutoff = v),
          ),
        ),
        _row(
          theme,
          label: '黑名单',
          hint:
              '一行一个域名。交给服务商去排除，不是在我们这侧过滤 ——'
              '本地过滤会让「回 5 条」变成「回 2 条」，而你以为是搜不到',
          child: TextField(
            controller: _deny,
            enabled: !_busy,
            minLines: 3,
            maxLines: 6,
            decoration: const InputDecoration(
              isDense: true,
              hintText: 'ads.example.com\ncontent-farm.example',
            ),
          ),
        ),

        const SizedBox(height: 20),
        _section(theme, 'URL 获取服务商'),
        _FetchProviders(prefs: p, chosen: _provider),

        if (_error case final err?) ...[
          const SizedBox(height: 14),
          Container(
            width: double.infinity,
            padding: const EdgeInsets.all(10),
            decoration: BoxDecoration(
              color: theme.colorScheme.errorContainer,
              borderRadius: BorderRadius.circular(CortexTokens.radiusRow),
            ),
            child: Text(
              err,
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onErrorContainer,
              ),
            ),
          ),
        ],

        const SizedBox(height: 18),
        Row(
          mainAxisAlignment: MainAxisAlignment.end,
          children: [
            FilledButton(
              key: const ValueKey('search:save'),
              onPressed: _busy
                  ? null
                  // 空的 key 框传 `null`（= 不动），不是空串（= 清掉）。
                  // 这两个在服务端是两件事，混了的话「改个结果个数」会把
                  // 用户的 key 清掉，而他没有明文可以补回来
                  : () => _save(
                      apiKey: _key.text.trim().isEmpty
                          ? null
                          : _key.text.trim(),
                    ),
              child: Text(_busy ? '保存中…' : '保存'),
            ),
          ],
        ),
      ],
    );
  }

  Widget _section(ThemeData theme, String title) => Padding(
    padding: const EdgeInsets.only(bottom: 8),
    child: Text(title, style: theme.textTheme.titleSmall),
  );

  Widget _row(
    ThemeData theme, {
    required String label,
    required String hint,
    required Widget child,
  }) => Padding(
    padding: const EdgeInsets.only(bottom: 14),
    child: Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        SizedBox(
          width: 150,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(label, style: theme.textTheme.labelLarge),
              const SizedBox(height: 2),
              Text(
                hint,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                  height: 1.5,
                ),
              ),
            ],
          ),
        ),
        const SizedBox(width: 16),
        Expanded(child: child),
      ],
    ),
  );
}

/// 顶上那一行：**这一刻到底能不能搜**。
///
/// 判据有两半，缺一半就会画出一个骗人的界面：选了一家但没填 key 时
/// 服务端**不会**回落到部署那把，所以「选了一家」不等于「能用」。
class _StatusLine extends StatelessWidget {
  const _StatusLine({required this.prefs});

  final SearchPrefs prefs;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final tokens = theme.cortex;
    final ok = prefs.usable;
    final (color, text) = ok
        ? (
            tokens.success,
            prefs.provider.isEmpty
                ? '正在用这个部署配好的那一份'
                : '正在用你自己配的 ${prefs.current?.name ?? prefs.provider}',
          )
        : (
            tokens.warning,
            prefs.provider.isEmpty
                // 这个部署没配、用户也没配 —— 说清「模型现在没有这个工具」，
                // 而不是含糊地说「未配置」
                ? '还没接上。模型此刻没有联网检索这个工具 —— 选一家并填上 key。'
                : '选了 ${prefs.current?.name ?? prefs.provider}，但还没填 key —— '
                      '这时不会回落到部署那把，模型仍然搜不了。',
          );

    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(CortexTokens.radiusRow),
        border: Border.all(color: color.withValues(alpha: 0.35)),
      ),
      child: Row(
        children: [
          Icon(
            ok ? Icons.check_circle_outline : Icons.info_outline,
            size: 17,
            color: color,
          ),
          const SizedBox(width: 9),
          Expanded(child: Text(text, style: theme.textTheme.bodySmall)),
        ],
      ),
    );
  }
}

/// 抓正文用哪家。
///
/// # 为什么它不是第二个下拉框
///
/// Cherry 那一页有两个下拉框，因为它认识的服务商里有好几家**只做抓取**
/// （`fetch`、Jina）。我们这边抓取能力只跟着搜索那一家走 —— 摆一个只有
/// 一项、且那一项由上面决定的下拉框，是一个点了没用的控件。
///
/// 所以这里画的是**结论**：你选的这家抓不抓得了正文。抓不了的时候要说清
/// 后果，否则用户会以为 `web_fetch` 坏了。
class _FetchProviders extends StatelessWidget {
  const _FetchProviders({required this.prefs, required this.chosen});

  final SearchPrefs prefs;
  final String chosen;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final names = prefs.fetchers.map((f) => f.name).join('、');

    // ⚠️ 按 [chosen]（下拉框里**当下**选的那个）去查，不是 `prefs.current`
    // （**已保存**的那个）。混着用的话，改了下拉框还没点保存时查不到，
    // 于是回落成「原始 id + 抓不了」—— 屏幕上就是
    // 「⚠️ exa 只做搜索……要抓正文的话，换成：Tavily、Exa」，
    // 同一句话既说它不行又叫你换成它。
    final picked = prefs.byId(chosen);
    final current = chosen.isEmpty
        // 部署那一档走的是服务端配的那家（目前必定是 Tavily），它抓得了
        ? (able: true, who: '部署提供的那一份')
        : (
            // 认不出这个 id 时按「抓不了」说 —— 下拉框是拿 providers 画的，
            // 走不到这里；真走到了也是保守的那一边
            able: picked?.canFetch ?? false,
            who: picked?.name ?? chosen,
          );

    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Text(
        current.able
            ? '${current.who} 抓得了网页正文，模型的 web_fetch 走它。'
                  '\n（支持抓取的服务商：$names）'
            : '⚠️ ${current.who} 只做搜索，不抓正文 —— 选它之后模型的 web_fetch 会明确告诉'
                  '用户这条路用不了，而不是悄悄换一家花你另一份额度。'
                  '\n要抓正文的话，换成：$names',
        style: theme.textTheme.bodySmall?.copyWith(
          color: current.able
              ? theme.cortex.foregroundTertiary
              : theme.cortex.warning,
          height: 1.6,
        ),
      ),
    );
  }
}

/// 加减一个数。比让人在文本框里打数字省事，也顺手把范围锁死了。
class _Stepper extends StatelessWidget {
  const _Stepper({
    required this.value,
    required this.min,
    required this.max,
    required this.enabled,
    required this.onChanged,
    this.step = 1,
  });

  final int value;
  final int min;
  final int max;
  final int step;
  final bool enabled;
  final void Function(int) onChanged;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Row(
      children: [
        IconButton(
          onPressed: enabled && value > min
              ? () => onChanged((value - step).clamp(min, max))
              : null,
          iconSize: 18,
          visualDensity: VisualDensity.compact,
          icon: const Icon(Icons.remove_rounded),
        ),
        SizedBox(
          width: 56,
          child: Text(
            '$value',
            textAlign: TextAlign.center,
            style: theme.textTheme.bodyMedium,
          ),
        ),
        IconButton(
          onPressed: enabled && value < max
              ? () => onChanged((value + step).clamp(min, max))
              : null,
          iconSize: 18,
          visualDensity: VisualDensity.compact,
          icon: const Icon(Icons.add_rounded),
        ),
      ],
    );
  }
}
