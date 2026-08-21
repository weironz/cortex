import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/theme.dart';
import '../../../models/usage_report.dart';
import '../../../state/app_providers.dart';
import '../../../widgets/empty_state.dart';
import '../widgets/settings_layout.dart';

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
          SettingsSection(
            title: '花费',
            // 「最近 N 天」这件事读数字的人一定会问，所以写在小标题下面，
            // 而不是压在整页最底下一行细字里（重排前它在那儿）
            description:
                '按最近 ${r.windowDays} 天滚动计算，不是自然月 —— '
                '自然月会让「这个月」的含义随时区漂移，也会在月初造出一个集中重置的尖峰。',
            trailing: IconButton(
              tooltip: '刷新',
              visualDensity: VisualDensity.compact,
              icon: const Icon(Icons.refresh_rounded, size: 16),
              onPressed: () => ref.invalidate(usageProvider),
            ),
            children: [SettingsCard(child: _Summary(report: r))],
          ),
          SettingsSection(
            title: '按模型',
            children: [
              if (r.byModel.isEmpty)
                SettingsNote(
                  child: Text(
                    '最近 ${r.windowDays} 天还没有调用记录。',
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                  ),
                )
              else
                SettingsCard(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 14,
                    vertical: 6,
                  ),
                  child: Column(
                    children: [
                      for (final m in r.byModel)
                        _ModelRow(usage: m, currency: r.currency),
                    ],
                  ),
                ),
              if (r.unpricedTokens > 0) ...[
                const SizedBox(height: 10),
                _UnpricedNote(tokens: r.unpricedTokens),
              ],
            ],
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
                '最近 ${report.windowDays} 天 · '
                '${formatTokens(report.usedTokens + report.ownKeyTokens)} token',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                ),
              ),
            ),
          ],
        ),
        const SizedBox(height: 14),
        if (ratio != null) ...[
          ClipRRect(
            borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
            child: LinearProgressIndicator(
              value: ratio,
              minHeight: 8,
              // 快用完时变色。到 90% 才提醒太晚了 —— 那时用户可能正在
              // 跑一件长活，而它会在中途停下
              color: ratio >= 0.9 ? scheme.error : scheme.primary,
              backgroundColor: scheme.surfaceContainerHighest,
            ),
          ),
          const SizedBox(height: 10),
          SettingsRow(
            label: '配额',
            value:
                '已用 ${formatTokens(report.usedTokens)} / '
                '${formatTokens(report.limitTokens!)}',
            note: '还剩 ${formatTokens(report.remainingTokens ?? 0)}',
          ),
        ] else
          SettingsRow(label: '配额', value: '不限', note: '这个部署不限额度。'),
        if (report.ownKeyTokens > 0)
          SettingsRow(
            label: '自带 key',
            value: '${formatTokens(report.ownKeyTokens)} token',
            note: '走的是你自己的 key，不占配额。',
          ),
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
    final cost = usage.costMicros;

    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 7),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  usage.model.isEmpty ? '（没记下模型名）' : usage.model,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.onSurface,
                  ),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
                Text(
                  '输入 ${formatTokens(usage.inputTokens)} · '
                  '输出 ${formatTokens(usage.outputTokens)}',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: theme.cortex.foregroundTertiary,
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
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
                fontStyle: FontStyle.italic,
              ),
            )
          else
            Text(
              formatMoney(cost, currency),
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurface,
                // 金额右对齐成一列要等宽，否则小数点参差不齐
                fontFamily: 'monospace',
              ),
            ),
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
    return SettingsNote(
      child: Text.rich(
        TextSpan(
          style: theme.textTheme.labelSmall?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
          children: [
            TextSpan(
              text: '有 ${formatTokens(tokens)} token 用的是没有价目的模型，上面那个金额里',
            ),
            const TextSpan(
              text: '不包含',
              style: TextStyle(fontWeight: FontWeight.w700),
            ),
            const TextSpan(text: '它们。要把它们算进去，在服务端配 CORTEX_MODEL_PRICES。'),
          ],
        ),
      ),
    );
  }
}
