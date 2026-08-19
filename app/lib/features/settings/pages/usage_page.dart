import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../models/usage_report.dart';
import '../../../state/app_providers.dart';
import '../../../widgets/empty_state.dart';

/// `GET /auth/usage`。
///
/// 每次翻到这一页重新拉：用量是一直在变的数，缓存住的话用户看到的是他
/// 上次打开设置那一刻的数字，而这一页存在的全部意义就是「现在用了多少」。
final usageProvider = FutureProvider.autoDispose<UsageReport>(
  (ref) => ref.watch(cortexApiProvider).usage(),
);

/// 用量这一页：这个窗口用了多少 token、花了多少钱、还剩多少额度。
///
/// # 为什么值得单独一页
///
/// 服务端从开放注册那天起就在记这笔账（`cortex_auth.usage`，每次 LLM
/// 调用一行），而在这一页之前**没有任何地方看得见它**。用户唯一会知道
/// 自己用了多少的时刻，是撞上配额被 429 拦下那一次 —— 那时已经晚了，
/// 而那条错误信息还得同时负责解释「配额是什么」。
class UsagePage extends ConsumerWidget {
  const UsagePage({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final async = ref.watch(usageProvider);

    return async.when(
      loading: () => const Center(
        child: SizedBox(
          width: 20,
          height: 20,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      ),
      error: (e, _) {
        // 没接账号体系的部署压根没有这条路。那不是错误，也不是用户能
        // 处理的事 —— 说清楚「这个部署不记账」比一条红色的 404 有用
        if (e is CortexApiException && e.isUnsupported) {
          return const EmptyState(
            icon: Icons.receipt_long_outlined,
            title: '这个部署不记用量',
            description: '用量与配额属于多用户部署。自托管单人时花的是自己的 key，服务端不替你记账。',
          );
        }
        return EmptyState(
          icon: Icons.cloud_off_rounded,
          title: '拉不到用量',
          description: e is CortexApiException ? e.message : '$e',
          tone: EmptyStateTone.error,
          action: OutlinedButton.icon(
            onPressed: () => ref.invalidate(usageProvider),
            icon: const Icon(Icons.refresh_rounded, size: 16),
            label: const Text('重试'),
          ),
        );
      },
      data: (r) => ListView(
        padding: const EdgeInsets.symmetric(horizontal: 4),
        children: [
          _Summary(report: r),
          const SizedBox(height: 20),
          Text('按模型', style: theme.textTheme.titleSmall),
          const SizedBox(height: 8),
          if (r.byModel.isEmpty)
            Text(
              '最近 ${r.windowDays} 天还没有调用记录。',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            )
          else
            ...r.byModel.map((m) => _ModelRow(usage: m, currency: r.currency)),
          if (r.unpricedTokens > 0) ...[
            const SizedBox(height: 12),
            _UnpricedNote(tokens: r.unpricedTokens),
          ],
          const SizedBox(height: 20),
          Text(
            '按最近 ${r.windowDays} 天滚动计算，不是自然月 —— '
            '自然月会让「这个月」的含义随时区漂移，也会在月初造出一个集中重置的尖峰。',
            style: theme.textTheme.labelSmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}

class _Summary extends StatelessWidget {
  const _Summary({required this.report});

  final UsageReport report;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final ratio = report.ratio;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            Text(
              formatMoney(report.costMicros, report.currency),
              style: theme.textTheme.headlineMedium,
            ),
            const SizedBox(width: 8),
            Padding(
              padding: const EdgeInsets.only(bottom: 4),
              child: Text(
                '最近 ${report.windowDays} 天',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ),
          ],
        ),
        const SizedBox(height: 4),
        Text(
          '${formatTokens(report.usedTokens + report.ownKeyTokens)} token',
          style: theme.textTheme.bodyMedium?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
        ),
        const SizedBox(height: 16),
        if (ratio != null) ...[
          ClipRRect(
            borderRadius: BorderRadius.circular(4),
            child: LinearProgressIndicator(
              value: ratio,
              minHeight: 8,
              // 快用完时变色。到 90% 才提醒太晚了 —— 那时用户可能正在
              // 跑一件长活，而它会在中途停下
              color: ratio >= 0.9 ? scheme.error : scheme.primary,
              backgroundColor: scheme.surfaceContainerHighest,
            ),
          ),
          const SizedBox(height: 8),
          Text(
            '配额：已用 ${formatTokens(report.usedTokens)} / '
            '${formatTokens(report.limitTokens!)}，'
            '还剩 ${formatTokens(report.remainingTokens ?? 0)}',
            style: theme.textTheme.bodySmall,
          ),
        ] else
          Text(
            '这个部署不限额度。',
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        if (report.ownKeyTokens > 0) ...[
          const SizedBox(height: 6),
          Text(
            '其中 ${formatTokens(report.ownKeyTokens)} token 走的是你自己的 key，不占配额。',
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
        ],
      ],
    );
  }
}

class _ModelRow extends StatelessWidget {
  const _ModelRow({required this.usage, required this.currency});

  final ModelUsage usage;
  final String currency;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final cost = usage.costMicros;

    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  usage.model.isEmpty ? '（没记下模型名）' : usage.model,
                  style: theme.textTheme.bodyMedium,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
                Text(
                  '输入 ${formatTokens(usage.inputTokens)} · '
                  '输出 ${formatTokens(usage.outputTokens)}',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: scheme.onSurfaceVariant,
                  ),
                ),
              ],
            ),
          ),
          const SizedBox(width: 12),
          // **算不出价时不显示 ¥0.00。** 那个数字读起来是「免费」，
          // 而事实是「这个部署没有这个模型的价目」
          if (cost == null)
            Text(
              '没有价目',
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
                fontStyle: FontStyle.italic,
              ),
            )
          else
            Text(formatMoney(cost, currency), style: theme.textTheme.bodyMedium),
        ],
      ),
    );
  }
}

class _UnpricedNote extends StatelessWidget {
  const _UnpricedNote({required this.tokens});

  final int tokens;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(Icons.info_outline_rounded, size: 16, color: scheme.onSurfaceVariant),
          const SizedBox(width: 8),
          Expanded(
            child: Text.rich(
              TextSpan(
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
                children: [
                  TextSpan(text: '有 ${formatTokens(tokens)} token 用的是没有价目的模型，上面那个金额里'),
                  const TextSpan(
                    text: '不包含',
                    style: TextStyle(fontWeight: FontWeight.w700),
                  ),
                  const TextSpan(text: '它们。要把它们算进去，在服务端配 CORTEX_MODEL_PRICES。'),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}
