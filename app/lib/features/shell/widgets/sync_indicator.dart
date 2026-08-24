import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../state/sync_controller.dart';

/// Realtime-link status, deliberately the quietest thing in the header.
///
/// It is a 7px dot. A banner or a toast would be wrong: the link dropping is
/// not something the user did or has to act on — the client reconnects by
/// itself and catches up from its own cursor, so the only honest UI is an
/// ambient one that can be checked when curious and ignored otherwise.
///
/// Hidden entirely on the mock source: there is no daemon, so "disconnected"
/// would be a lie and "connected" would be worse.
class SyncIndicator extends ConsumerWidget {
  const SyncIndicator({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final state = ref.watch(syncControllerProvider);
    if (state.status == SyncLinkStatus.disabled) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final (Color color, IconData? icon) = switch (state.status) {
      SyncLinkStatus.live when state.catchingUp || state.isBehind => (
        scheme.secondary,
        null,
      ),
      // 绿 = 已追平。取语义 token 而不是写死色值：深浅主题各配了一档，
      // 硬编码的那一个在深色底上对比不够，也没人会记得同步它
      SyncLinkStatus.live => (theme.cortex.success, null),
      SyncLinkStatus.connecting => (scheme.onSurfaceVariant, null),
      SyncLinkStatus.reconnecting => (scheme.error, Icons.sync_problem_rounded),
      SyncLinkStatus.disabled => (scheme.onSurfaceVariant, null),
    };

    return Tooltip(
      message: _tooltip(state),
      child: InkResponse(
        // Only meaningful while down; tapping a healthy link would just churn
        // the connection.
        onTap: state.status == SyncLinkStatus.reconnecting
            ? ref.read(syncControllerProvider.notifier).reconnectNow
            : null,
        radius: 14,
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 8),
          child: icon != null
              ? Icon(icon, size: 13, color: color)
              : Container(
                  width: 7,
                  height: 7,
                  decoration: BoxDecoration(
                    color: color,
                    shape: BoxShape.circle,
                  ),
                ),
        ),
      ),
    );
  }

  static String _tooltip(SyncState s) {
    final counters =
        'bump ${s.bumps} · resync ${s.resyncs}'
        '${s.resyncs > 0 ? '（resync 说明服务端可能漏推过，属运维信号）' : ''}';

    return switch (s.status) {
      SyncLinkStatus.disabled => 'Mock 数据源没有实时通道',
      SyncLinkStatus.connecting => '正在连接实时同步…',
      SyncLinkStatus.reconnecting => [
        '实时同步已断开${s.attempt > 1 ? '（已重试 ${s.attempt} 次）' : ''}',
        if (s.nextRetryAt != null)
          '${_secondsUntil(s.nextRetryAt!)} 秒后自动重连，点击立即重试',
        if (s.error != null) s.error!,
      ].join('\n'),
      SyncLinkStatus.live => [
        s.catchingUp
            ? '实时同步已连接 · 正在追平'
            : s.isBehind
            ? '实时同步已连接 · 落后 ${s.serverCursor - s.cursor} 条'
            : '实时同步已连接 · 已追平',
        '游标 ${s.cursor} / 服务端 ${s.serverCursor}',
        counters,
        if (s.version != null) 'cortexd v${s.version}',
      ].join('\n'),
    };
  }

  static int _secondsUntil(DateTime when) {
    final left = when.difference(DateTime.now()).inMilliseconds;
    return left <= 0 ? 0 : (left / 1000).ceil();
  }
}
