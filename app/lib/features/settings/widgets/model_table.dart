/// 一条来源的型号列表 —— 照 LobeHub 的「模型列表」。
///
/// # 它解决的问题
///
/// 上一版是「拉完列表 → 弹一个对话框勾选 → 勾中的进列表」。那样有两个洞：
///
/// 1. **没勾的型号不在任何地方。** 想找回来只能重新拉一次列表；
/// 2. 列表里只有名字，没有价格、上下文、能力，而这三样正是「该开哪个」
///    的全部判据 —— 用户得去供应商官网另外查一遍。
///
/// 现在服务端记着「这家有哪些」（`ModelSource.catalog`），
/// 列表把开着的和没开的一起摆出来，每行带上判据，开关就地拨。
library;

import 'package:flutter/material.dart';

import '../../../core/theme.dart';
import '../../../models/model_source.dart';
import 'model_format.dart';
import 'settings_layout.dart';

class ModelTable extends StatefulWidget {
  const ModelTable({
    super.key,
    required this.source,
    required this.busy,
    required this.onToggle,
    required this.onFetch,
  });

  final ModelSource source;
  final bool busy;

  /// 开 / 关一个型号。调用方负责落库并重拉。
  final void Function(String modelId, bool on) onToggle;

  final VoidCallback onFetch;

  @override
  State<ModelTable> createState() => _ModelTableState();
}

class _ModelTableState extends State<ModelTable> {
  String _query = '';

  /// `null` = 全部。
  ModelKind? _kind;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final s = widget.source;
    // `shownCatalog` 而不是 `catalog`：老服务端不下发那个字段，
    // 直接读会让一条**配着型号的**来源显示成「还没有型号」，
    // 而用户上一分钟还在用它们聊天
    final all = s.shownCatalog;

    // 页签的计数**按「全部」算，不按当前筛选算** —— 否则点进「图片」
    // 之后「对话」会显示 0，看起来像那一档空了
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

    final on = shown.where((m) => s.isEnabled(m.id)).toList();
    final off = shown.where((m) => !s.isEnabled(m.id)).toList();

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _header(context, all.length),
        if (all.isEmpty)
          SettingsNote(
            child: Text(
              // 不拿内置目录顶替：目录知道这家有几十个型号，但这个账号未必
              // 都开通了。填进选择器的每一个都必须是真的调得通的
              '还没有型号。点「获取模型列表」去问这家它到底开放了哪些 —— '
              '内置那份是编译期写死的，未必与你的账号一致。',
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          )
        else ...[
          _tabs(context, counts, all.length),
          const SizedBox(height: 8),
          if (shown.isEmpty)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 18),
              child: Center(
                child: Text(
                  '没有匹配的型号',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: theme.cortex.foregroundTertiary,
                  ),
                ),
              ),
            )
          else
            SettingsCard(
              padding: const EdgeInsets.fromLTRB(14, 4, 8, 8),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  // **「已启用」永远画，哪怕是空的。** 一条来源刚配好时它
                  // 就是空的，而那正是用户最需要被告知「去下面打开几个」
                  // 的时刻 —— 不画的话他只看到一长串灰的，不知道该干嘛
                  _group(context, '已启用', on.length),
                  // ⚠️ **判据是 `s.models.isEmpty`，不是 `on.isEmpty`。**
                  //
                  // `on` 是**过滤之后**的。用它当判据的话，搜一个不匹配任何
                  // 已启用型号的词（比如这条来源开着 gpt-image-2 而你搜
                  // 「gemini」），这里就会写「一个都没开」—— 而它明明开着一个。
                  // 2026-08-21 实测撞到，一句凭空的假话。
                  if (s.models.isEmpty)
                    Padding(
                      padding: const EdgeInsets.only(top: 2, bottom: 6),
                      child: Text(
                        '一个都没开。在下面打开你要用的那几个，它们才会出现在对话的模型选择器里。',
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: theme.cortex.foregroundTertiary,
                        ),
                      ),
                    )
                  else
                    // 开着的被筛掉时这里什么都不画：标题上那个「（0）」
                    // 已经说清了「当前筛选下没有」，再补一句会说成别的意思
                    for (final m in on) _row(context, m, true),
                  // 「未启用」空着就不画：那时它不传达任何信息
                  if (off.isNotEmpty) ...[
                    _group(context, '未启用', off.length),
                    for (final m in off) _row(context, m, false),
                  ],
                ],
              ),
            ),
        ],
      ],
    );
  }

  Widget _header(BuildContext context, int total) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        children: [
          Text('模型列表', style: theme.textTheme.titleSmall),
          const SizedBox(width: 6),
          if (total > 0)
            Text(
              '$total',
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
          const Spacer(),
          if (total > 0)
            SizedBox(
              width: 160,
              child: TextField(
                onChanged: (v) => setState(() => _query = v),
                autocorrect: false,
                style: theme.textTheme.bodySmall,
                decoration: const InputDecoration(
                  isDense: true,
                  hintText: '搜索模型',
                  prefixIcon: Icon(Icons.search, size: 15),
                  prefixIconConstraints: BoxConstraints(minWidth: 30),
                  border: OutlineInputBorder(),
                ),
              ),
            ),
          const SizedBox(width: 8),
          TextButton.icon(
            onPressed: widget.busy ? null : widget.onFetch,
            icon: const Icon(Icons.refresh, size: 15),
            label: const Text('获取模型列表'),
          ),
        ],
      ),
    );
  }

  /// 模态页签。**只画数得出来的那几档**，见 [ModelKind]。
  Widget _tabs(BuildContext context, Map<ModelKind, int> counts, int total) {
    final present = ModelKind.values
        .where((k) => (counts[k] ?? 0) > 0)
        .toList();
    // 只有一档时整排页签都是噪音：点哪个结果都一样
    if (present.length < 2) return const SizedBox.shrink();

    return Row(
      children: [
        _tab(context, null, Icons.apps_rounded, '全部', total),
        for (final k in present) ...[
          const SizedBox(width: 4),
          _tab(context, k, k.icon, k.label, counts[k] ?? 0),
        ],
      ],
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
        padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 5),
        decoration: BoxDecoration(
          // 中性（规范第九节）：这是「我在看哪一档」，位置不是动作
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
              '$label ($count)',
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

  Widget _group(BuildContext context, String label, int count) {
    final theme = Theme.of(context);
    return Padding(
      // 上 12 下 2：它属于**下面**那一组，均分会让它读成两组之间的一条线
      padding: const EdgeInsets.fromLTRB(0, 12, 0, 2),
      child: Text(
        '$label（$count）',
        style: theme.textTheme.labelSmall?.copyWith(
          fontWeight: FontWeight.w600,
          color: theme.cortex.foregroundTertiary,
        ),
      ),
    );
  }

  Widget _row(BuildContext context, FetchedModel m, bool on) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final price = formatPricePair(m);
    final ctx = formatContextTokens(m.context);

    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 5),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Flexible(
                      child: Text(
                        m.label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodySmall?.copyWith(
                          // 关着的整行退到第三级：它还在，但不参与对话
                          color: on
                              ? scheme.onSurface
                              : theme.cortex.foregroundTertiary,
                        ),
                      ),
                    ),
                    // 显示名与 id 不同才画 id。相同时画两遍是纯噪音，
                    // 而这一列里大多数型号的显示名就是它的 id
                    if (m.displayName.isNotEmpty && m.displayName != m.id) ...[
                      const SizedBox(width: 6),
                      _IdChip(id: m.id),
                    ],
                  ],
                ),
                // 价格查不到就整行不画（**不画 `—`**：一个横杠在一列
                // 数字里读起来像「免费」）
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
          const SizedBox(width: 8),
          for (final b in badgesOf(m))
            Padding(
              padding: const EdgeInsets.only(left: 5),
              child: Tooltip(
                message: b.$2,
                child: Icon(b.$1, size: 13, color: scheme.onSurfaceVariant),
              ),
            ),
          if (ctx.isNotEmpty) ...[
            const SizedBox(width: 8),
            Tooltip(
              message: '上下文窗口 ${m.context} token',
              child: Container(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
                decoration: BoxDecoration(
                  color: scheme.surfaceContainerHighest,
                  borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
                ),
                child: Text(ctx, style: theme.textTheme.labelSmall),
              ),
            ),
          ],
          const SizedBox(width: 4),
          Switch(
            value: on,
            onChanged: widget.busy ? null : (v) => widget.onToggle(m.id, v),
          ),
        ],
      ),
    );
  }
}

/// 型号 id 的小标签。
class _IdChip extends StatelessWidget {
  const _IdChip({required this.id});

  final String id;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 5, vertical: 1),
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
      ),
      child: Text(
        id,
        style: theme.textTheme.labelSmall?.copyWith(
          // 填进请求里的就是它，要能逐字符核对
          fontFamily: 'monospace',
          color: theme.cortex.foregroundTertiary,
        ),
      ),
    );
  }
}
