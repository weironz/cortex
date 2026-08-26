/// 一条来源的型号列表 —— **只放你在用的那几个**，照 Cherry Studio。
///
/// # 它的两次形态，以及为什么落在这一版
///
/// 1. 最早是「拉完列表 → 弹对话框勾选 → 勾中的进列表」。洞是**没勾的型号
///    不在任何地方**，想找回来只能重新拉一次。
/// 2. 然后照 LobeHub 把全集摊在这里，分 已启用 / 未启用 两组、每行一个开关。
///    洞是**长**：实测一把中转站的 key 拉回来 281 个，于是这一列变成 280 行
///    灰的，真正在用的那一两个被埋在最上面。
///
/// 现在按 Cherry 的分法：这里只放已启用的（按系列分组、每行一个「−」），
/// 全集在「获取模型列表」弹出的右侧抽屉里（见 `model_picker_drawer.dart`）。
/// 「我平时看什么」与「我偶尔来挑一个」是两件事，分开之后前者永远短。
///
/// 服务端那个 `catalog` 字段没有白做 —— 它现在是抽屉的数据源，
/// 而「关掉的型号还找得回来」这条仍然成立。
library;

import 'package:flutter/material.dart';

import '../../../core/theme.dart';
import '../../../models/model_source.dart';
import 'model_format.dart';
import 'settings_layout.dart';

class ModelTable extends StatelessWidget {
  const ModelTable({
    super.key,
    required this.source,
    required this.busy,
    required this.onToggle,
    required this.onFetch,
    this.onEdit,
  });

  final ModelSource source;
  final bool busy;

  /// 开 / 关一个型号。调用方负责落库并重拉。
  final void Function(String modelId, bool on) onToggle;

  /// 点「获取模型列表」—— 调用方去拉，然后把抽屉打开。
  final VoidCallback onFetch;

  /// 编辑这个型号的能力。`null` = 这条来源改不了（部署提供那条）。
  ///
  /// # 为什么这里必须也有一个入口
  ///
  /// 齿轮原本只在「获取模型列表」那个抽屉里。而**这一列才是人真正看模型
  /// 的地方** —— 它列的是你已经启用、每天在用的那几个；抽屉里是几百行
  /// 全集，是「去挑一个新的」时才开的。
  ///
  /// 只把入口放在抽屉里，等于让用户为了改一个眼前的模型，先去拉一次全集、
  /// 再在几百行里把同一个型号找出来。2026-08-26 实地点这一页时发现的。
  final void Function(FetchedModel model)? onEdit;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    // 全集里查得到的用富信息，查不到的（手动加的、目录里没有的）
    // 也要画出来 —— 它确实在这条来源的列表里
    final byId = {for (final m in source.shownCatalog) m.id: m};
    final on = [
      for (final id in source.models) byId[id] ?? FetchedModel(id: id),
    ];

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _header(context, on.length),
        if (on.isEmpty)
          SettingsNote(
            child: Text(
              source.shownCatalog.isEmpty
                  // 不拿内置目录顶替：目录知道这家有几十个型号，但这个账号
                  // 未必都开通了。填进选择器的每一个都必须是真的调得通的
                  ? '还没有型号。点「获取模型列表」去问这家它到底开放了哪些 —— '
                        '内置那份是编译期写死的，未必与你的账号一致。'
                  : '一个都没加。点「获取模型列表」，从右边挑几个加进来 —— '
                        '加进来的才会出现在对话的模型选择器里。',
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          )
        else
          SettingsCard(
            padding: const EdgeInsets.fromLTRB(14, 4, 8, 10),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                for (final g in groupByFamily(on, (m) => m.id)) ...[
                  _family(context, g.$1, g.$2.length),
                  for (final m in g.$2) _row(context, m),
                ],
              ],
            ),
          ),
      ],
    );
  }

  Widget _header(BuildContext context, int count) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        children: [
          Text('模型', style: theme.textTheme.titleSmall),
          const SizedBox(width: 6),
          if (count > 0)
            Text(
              '$count',
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
          const Spacer(),
          TextButton.icon(
            onPressed: busy ? null : onFetch,
            icon: const Icon(Icons.refresh, size: 15),
            label: const Text('获取模型列表'),
          ),
        ],
      ),
    );
  }

  Widget _family(BuildContext context, String name, int count) {
    final theme = Theme.of(context);
    return Padding(
      // 上 10 下 2：组标题属于**下面**那一组
      padding: const EdgeInsets.fromLTRB(0, 10, 0, 2),
      child: Text(
        '$name（$count）',
        style: theme.textTheme.labelSmall?.copyWith(
          fontWeight: FontWeight.w600,
          color: theme.cortex.foregroundTertiary,
        ),
      ),
    );
  }

  Widget _row(BuildContext context, FetchedModel m) {
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
                          color: scheme.onSurface,
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
                // 价格查不到就整行不画（**不画 `—`**：一个横杠在一列数字里
                // 读起来像「免费」）
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
          const SizedBox(width: 2),
          // 齿轮摆在「−」左边：那一个回答「还要不要它」，这一个回答
          // 「它是什么」。前者更常用，留在最外侧最好点（与抽屉里那一行
          // 的顺序一致 —— 同一个动作在两处位置不同，手会记错）
          if (onEdit case final edit?)
            IconButton(
              tooltip: m.overridden.isEmpty ? '改这个模型的能力' : '这个模型的能力被你改过',
              iconSize: 16,
              visualDensity: VisualDensity.compact,
              onPressed: busy ? null : () => edit(m),
              icon: Icon(
                Icons.tune_rounded,
                // 改过的标出来：一列里哪几个是自己动过的要一眼看得见
                color: m.overridden.isEmpty ? null : theme.cortex.accentInk,
              ),
            ),
          // 「−」而不是开关：这一列里的每一个都是**开着**的，
          // 一排恒为「开」的开关不传达任何信息，而它占的宽度不小
          IconButton(
            tooltip: '从列表里移除（还能在「获取模型列表」里加回来）',
            iconSize: 18,
            visualDensity: VisualDensity.compact,
            onPressed: busy ? null : () => onToggle(m.id, false),
            icon: const Icon(Icons.remove_rounded),
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
          fontFamily: 'JetBrains Mono',
          fontFamilyFallback: CortexTheme.monoFallback,
          color: theme.cortex.foregroundTertiary,
        ),
      ),
    );
  }
}
