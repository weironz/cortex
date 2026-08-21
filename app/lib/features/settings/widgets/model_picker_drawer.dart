/// 「获取模型列表」弹出来的那个右侧抽屉 —— 照 Cherry Studio。
///
/// # 为什么全集要挪到抽屉里
///
/// 上一版（照 LobeHub）把全集摊在主列表里，分 已启用 / 未启用 两组。
/// 那在几十个型号时好看，而实测一把中转站的 key 拉回来 **281 个** ——
/// 于是主列表变成 280 行灰的，用户真正在用的那一两个被埋在最上面一行。
///
/// Cherry 的分法是：**主列表只放你在用的**，全集在一个随叫随到的抽屉里。
/// 「我平时看什么」与「我偶尔来挑一个」是两件事，分成两个地方之后，
/// 前者永远短。
///
/// # 分类页签只画数得出来的那几档
///
/// Cherry 那排有 `文本 / 图片 / 嵌入 / 音频 / 视频 / 重排`，而且**零也画**。
/// 我们只判得出「对话 / 图片」两档（见 [ModelKind]），所以只画这两档 ——
/// 画一个恒为 0 的页签，是在说一个不成立的能力。
library;

import 'package:flutter/material.dart';

import '../../../core/theme.dart';
import '../../../models/model_source.dart';
import 'model_format.dart';
import 'provider_mark.dart';

class ModelPickerDrawer extends StatefulWidget {
  const ModelPickerDrawer({
    super.key,
    required this.title,
    required this.provider,
    required this.catalog,
    required this.enabled,
    required this.busy,
    required this.onToggle,
    required this.onAddAll,
  });

  /// 抽屉标题，如「OpenAI 模型」。
  final String title;

  /// 画品牌标用。
  final String provider;

  /// 这条来源的全集。
  final List<FetchedModel> catalog;

  /// 已经加进来的那些 id。
  final List<String> enabled;

  final bool busy;

  /// 加/移一个型号。
  final void Function(String modelId, bool on) onToggle;

  /// 把当前筛选下**看得见的那些**一次全加进来。
  final void Function(List<String> ids) onAddAll;

  @override
  State<ModelPickerDrawer> createState() => _ModelPickerDrawerState();
}

class _ModelPickerDrawerState extends State<ModelPickerDrawer> {
  String _query = '';

  /// `null` = 全部。
  ModelKind? _kind;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final all = widget.catalog;

    // 页签计数按**全集**算，不按当前筛选 —— 否则点进「图片」之后
    // 「对话」会显示 0，看起来像那一档空了
    final counts = <ModelKind, int>{};
    for (final m in all) {
      counts.update(ModelKind.of(m), (n) => n + 1, ifAbsent: () => 1);
    }

    final keep = _query.trim().toLowerCase();
    final shown = all.where((m) {
      if (_kind != null && ModelKind.of(m) != _kind) return false;
      if (keep.isEmpty) return true;
      return m.id.toLowerCase().contains(keep) ||
          m.label.toLowerCase().contains(keep);
    }).toList();

    // 「添加全部」只加**当前看得见**的那些。筛了「图片」还全加进去，
    // 就是拿一个用户没看过的清单替他做决定
    final addable = [
      for (final m in shown)
        if (!widget.enabled.contains(m.id)) m.id,
    ];

    return Drawer(
      width: 460,
      backgroundColor: theme.colorScheme.surface,
      // 抽屉靠边贴着，右侧那两个角圆着没有意义
      shape: const RoundedRectangleBorder(),
      child: SafeArea(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _header(context, all.length, addable),
            _search(context),
            _tabs(context, counts, all.length),
            const SizedBox(height: 4),
            Expanded(
              child: shown.isEmpty
                  ? Center(
                      child: Text(
                        keep.isEmpty ? '这条来源还没有型号' : '没有匹配的型号',
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: theme.cortex.foregroundTertiary,
                        ),
                      ),
                    )
                  : ListView(
                      padding: const EdgeInsets.fromLTRB(12, 4, 12, 16),
                      children: [
                        for (final g in groupByFamily(shown, (m) => m.id)) ...[
                          _family(context, g.$1, g.$2.length),
                          for (final m in g.$2) _row(context, m),
                        ],
                      ],
                    ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _header(BuildContext context, int total, List<String> addable) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 12, 8, 8),
      child: Row(
        children: [
          ProviderMark(
            provider: widget.provider,
            displayName: widget.title,
            size: 20,
          ),
          const SizedBox(width: 8),
          // **标题要能被挤**：抽屉只有 460 宽，而右边那两个控件是定宽的。
          // 不给 Flexible 的话，一个长一点的来源名就把这一行撑溢出
          //（实测「阿里云百炼」那种长度就溢了 5.6px，画面上是一条黄黑条）
          Flexible(
            child: Text(
              widget.title,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.titleSmall,
            ),
          ),
          const SizedBox(width: 6),
          Text(
            '$total',
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.cortex.foregroundTertiary,
            ),
          ),
          const SizedBox(width: 8),
          // 一个都加不了时按不动（全加过了、或者筛空了）——
          // 一个点了没反应的按钮比没有这个按钮更让人困惑
          TextButton(
            onPressed: widget.busy || addable.isEmpty
                ? null
                : () => widget.onAddAll(addable),
            child: Text('添加全部 ${addable.length}'),
          ),
          IconButton(
            tooltip: '关闭',
            iconSize: 18,
            onPressed: () => Navigator.of(context).maybePop(),
            icon: const Icon(Icons.close_rounded),
          ),
        ],
      ),
    );
  }

  Widget _search(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
    child: TextField(
      onChanged: (v) => setState(() => _query = v),
      autocorrect: false,
      style: Theme.of(context).textTheme.bodySmall,
      decoration: const InputDecoration(
        isDense: true,
        hintText: '搜索模型…',
        prefixIcon: Icon(Icons.search, size: 16),
        border: OutlineInputBorder(),
      ),
    ),
  );

  Widget _tabs(BuildContext context, Map<ModelKind, int> counts, int total) {
    final present = ModelKind.values
        .where((k) => (counts[k] ?? 0) > 0)
        .toList();
    // 只有一档时整排页签都是噪音：点哪个结果都一样
    if (present.length < 2) return const SizedBox.shrink();

    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 0, 16, 4),
      child: Row(
        children: [
          _tab(context, null, Icons.apps_rounded, '全部', total),
          for (final k in present) ...[
            const SizedBox(width: 4),
            _tab(context, k, k.icon, k.label, counts[k] ?? 0),
          ],
        ],
      ),
    );
  }

  Widget _tab(
    BuildContext context,
    ModelKind? kind,
    IconData icon,
    String label,
    int count,
  ) {
    final theme = Theme.of(context);
    final selected = _kind == kind;
    return InkWell(
      onTap: () => setState(() => _kind = kind),
      borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 5),
        decoration: BoxDecoration(
          color: selected ? theme.cortex.sidebarAccent : Colors.transparent,
          borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(
              icon,
              size: 13,
              color: selected
                  ? theme.colorScheme.onSurface
                  : theme.cortex.foregroundTertiary,
            ),
            const SizedBox(width: 5),
            Text(
              '$label $count',
              style: theme.textTheme.labelSmall?.copyWith(
                color: selected
                    ? theme.colorScheme.onSurface
                    : theme.cortex.foregroundTertiary,
                fontWeight: selected ? FontWeight.w600 : null,
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _family(BuildContext context, String name, int count) {
    final theme = Theme.of(context);
    return Padding(
      // 上 12 下 2：组标题属于**下面**那一组，均分会让它读成两组之间的一条线
      padding: const EdgeInsets.fromLTRB(4, 12, 4, 2),
      child: Row(
        children: [
          Text(
            name,
            style: theme.textTheme.labelSmall?.copyWith(
              fontWeight: FontWeight.w600,
              color: theme.cortex.foregroundTertiary,
            ),
          ),
          const SizedBox(width: 6),
          Text(
            '$count',
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.cortex.foregroundTertiary,
            ),
          ),
        ],
      ),
    );
  }

  Widget _row(BuildContext context, FetchedModel m) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final on = widget.enabled.contains(m.id);
    final price = formatPricePair(m);
    final ctx = formatContextTokens(m.context);

    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 3),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  m.label,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: scheme.onSurface,
                  ),
                ),
                if (price.isNotEmpty)
                  Text(
                    price,
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: theme.cortex.foregroundTertiary,
                    ),
                  ),
              ],
            ),
          ),
          for (final b in badgesOf(m))
            Padding(
              padding: const EdgeInsets.only(left: 4),
              child: Tooltip(
                message: b.$2,
                child: Icon(b.$1, size: 13, color: scheme.onSurfaceVariant),
              ),
            ),
          if (ctx.isNotEmpty) ...[
            const SizedBox(width: 6),
            Text(
              ctx,
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
          ],
          const SizedBox(width: 4),
          // 加过的显示「−」（移除），没加的显示「+」。**同一个位置**，
          // 因为它回答的是同一个问题：这个型号在不在我的列表里
          IconButton(
            tooltip: on ? '从列表里移除' : '加进列表',
            iconSize: 18,
            visualDensity: VisualDensity.compact,
            onPressed: widget.busy ? null : () => widget.onToggle(m.id, !on),
            icon: Icon(on ? Icons.remove_rounded : Icons.add_rounded),
          ),
        ],
      ),
    );
  }
}
