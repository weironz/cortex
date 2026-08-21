/// 「全部」那一屏 —— 照 LobeHub 的服务商卡片墙。
///
/// # 与 LobeHub 的一处**模型差异**，必须说清
///
/// LobeHub 是**一家供应商一张卡**。我们不是：一家可以配**两条来源**
/// （两个网关、两个账号、一个官方一个中转），`ModelSource.label` 就是
/// 为此存在的。所以这里是**一条来源一张卡**。
///
/// 直接后果是「未启用」那一组里混着两种东西，它们能做的事不一样：
///
/// | 卡片背后 | 开关 | 点它 |
/// |---|---|---|
/// | 一条关掉的来源 | 有 —— 就地开 | 跳去它的详情 |
/// | 一家还没配过的供应商 | **没有** | 打开「添加」对话框 |
///
/// ⚠️ 第二种**不画开关**：没有 key 的供应商是启用不了的，
/// 画一个点了会弹窗的开关，是让控件说了一件它做不到的事。
library;

import 'package:flutter/material.dart';

import '../../../core/theme.dart';
import '../../../models/model_source.dart';
import 'provider_mark.dart';

class ProviderOverview extends StatelessWidget {
  const ProviderOverview({
    super.key,
    required this.data,
    required this.busy,
    required this.query,
    required this.onOpen,
    required this.onToggle,
    required this.onAdd,
  });

  final ModelSources data;
  final bool busy;

  /// 左列那个搜索框里的字。**两边一起过滤** —— 只过滤左列的话，
  /// 搜完之后右边这面墙纹丝不动，读起来像搜索没生效。
  final String query;

  /// 点开一条已有的来源 → 切到它的详情。
  final ValueChanged<String> onOpen;

  final void Function(ModelSource, bool) onToggle;

  /// 添加一条属于这家供应商的来源。
  final ValueChanged<ProviderChoice> onAdd;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final keep = query.trim().toLowerCase();
    bool hit(String name, String provider) =>
        keep.isEmpty ||
        name.toLowerCase().contains(keep) ||
        provider.toLowerCase().contains(keep);

    final matched = data.sources
        .where((s) => hit(data.nameOf(s), s.provider))
        .toList();
    final on = matched.where((s) => s.enabled).toList();
    final off = matched.where((s) => !s.enabled).toList();

    // 一条来源都没配过的供应商。已经配过的不再出现在这里 ——
    // 否则「阿里云百炼」会同时以「已启用」和「可添加」两张卡出现，
    // 而用户看不出它们是不是同一个东西
    final configured = data.sources.map((s) => s.provider).toSet();
    final fresh = data.providers
        .where((p) => !configured.contains(p.id) && hit(p.displayName, p.id))
        .toList();

    return ListView(
      padding: const EdgeInsets.symmetric(horizontal: 4),
      children: [
        _section(context, '已启用服务商', on.length),
        if (on.isEmpty)
          _emptyNote(theme, '一条都没开。下面挑一家填上 key 就能用。')
        else
          _grid(context, [for (final s in on) _sourceCard(context, s)]),
        const SizedBox(height: 24),
        _section(context, '未启用服务商', off.length + fresh.length),
        _grid(context, [
          for (final s in off) _sourceCard(context, s),
          for (final p in fresh) _freshCard(context, p),
        ]),
      ],
    );
  }

  Widget _section(BuildContext context, String title, int count) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 12),
      child: Row(
        children: [
          Text(title, style: theme.textTheme.titleSmall),
          const SizedBox(width: 8),
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
            decoration: BoxDecoration(
              color: theme.colorScheme.surfaceContainerHighest,
              borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
            ),
            child: Text('$count', style: theme.textTheme.labelSmall),
          ),
        ],
      ),
    );
  }

  Widget _emptyNote(ThemeData theme, String text) => Padding(
    padding: const EdgeInsets.only(bottom: 4),
    child: Text(
      text,
      style: theme.textTheme.labelSmall?.copyWith(
        color: theme.cortex.foregroundTertiary,
      ),
    ),
  );

  /// 卡片按可用宽度铺，**每张最窄 240**。
  ///
  /// 不写死列数：这一屏的宽度跟着设置窗走，而设置窗跟着屏幕走
  /// （见 `settings_sheet.dart`）—— 写死三列的话，窄窗上每张卡会被挤成
  /// 一条只放得下半个名字的缝
  Widget _grid(BuildContext context, List<Widget> cards) {
    if (cards.isEmpty) return const SizedBox.shrink();
    return LayoutBuilder(
      builder: (context, box) {
        final columns = (box.maxWidth / 240).floor().clamp(1, 4);
        const gap = 12.0;
        final width = (box.maxWidth - gap * (columns - 1)) / columns;
        return Wrap(
          spacing: gap,
          runSpacing: gap,
          children: [for (final c in cards) SizedBox(width: width, child: c)],
        );
      },
    );
  }

  Widget _sourceCard(BuildContext context, ModelSource s) {
    final theme = Theme.of(context);
    return _Card(
      // 与左列那一行区分：同一条来源在这一屏上有两个可点的落点
      key: ValueKey('card:${s.id}'),
      onTap: () => onOpen(s.id),
      mark: ProviderMark(
        provider: s.provider,
        displayName: data.nameOf(s),
        size: 22,
      ),
      title: data.nameOf(s),
      // 副标题说**这条**的实况，不是供应商的宣传语：同一家配两条时，
      // 两张卡的宣传语一模一样，而 key 尾巴与型号数正是分辨它们的东西
      description: s.builtin
          ? '这条是服务端配的，免费但占配额。'
          : '你的 key …${s.keyTail} · ${s.models.length} 个型号已启用',
      // 部署那条也有开关：它**可以关**（一个自带 key 的人未必愿意花配额），
      // 只是不能删 —— 「关掉」与「删掉」在这条上是两件事
      trailing: _switch(context, s),
      footer: s.enabled
          ? null
          : Text(
              '关着 —— 它不进模型选择器，配置留着。',
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
    );
  }

  Widget _switch(BuildContext context, ModelSource s) =>
      Switch(value: s.enabled, onChanged: busy ? null : (v) => onToggle(s, v));

  /// 还没配过的一家。**没有开关** —— 见文件头那张表。
  Widget _freshCard(BuildContext context, ProviderChoice p) {
    final theme = Theme.of(context);
    return _Card(
      key: ValueKey('card:new:${p.id}'),
      onTap: busy ? null : () => onAdd(p),
      mark: ProviderMark(provider: p.id, displayName: p.displayName, size: 22),
      title: p.displayName,
      description: p.description.isEmpty ? '点这里填上 key 就能用。' : p.description,
      trailing: Icon(
        Icons.add_rounded,
        size: 18,
        color: theme.cortex.foregroundTertiary,
      ),
    );
  }
}

/// 卡片本体。表面走 card 那一档、圆角走 XL、边框带透明度（规范第二、四节）。
class _Card extends StatelessWidget {
  const _Card({
    super.key,
    required this.mark,
    required this.title,
    required this.description,
    this.trailing,
    this.footer,
    this.onTap,
  });

  final Widget mark;
  final String title;
  final String description;
  final Widget? trailing;
  final Widget? footer;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Material(
      color: scheme.surfaceContainer,
      borderRadius: BorderRadius.circular(CortexTokens.radiusXl),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(CortexTokens.radiusXl),
        child: Container(
          padding: const EdgeInsets.fromLTRB(14, 12, 10, 12),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(CortexTokens.radiusXl),
            border: Border.all(color: scheme.outlineVariant),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  mark,
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodyMedium?.copyWith(
                        fontWeight: FontWeight.w600,
                        color: scheme.onSurface,
                      ),
                    ),
                  ),
                  ?trailing,
                ],
              ),
              const SizedBox(height: 8),
              Text(
                description,
                // 两行封顶：卡片墙的价值是**一眼扫过一排**，
                // 一段长简介会把网格撑成参差不齐的高度
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                  height: 1.5,
                ),
              ),
              if (footer != null) ...[const SizedBox(height: 6), footer!],
            ],
          ),
        ),
      ),
    );
  }
}
