import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../../widgets/panel_header.dart';
import '../shell/widgets/sync_indicator.dart';
import '../shell/widgets/update_indicator.dart';
import '../workspace/workspace_panel.dart';
import 'widgets/confirm_panel.dart';
import 'widgets/conversation_view.dart';
import 'widgets/message_composer.dart';

/// Centre pane: header, transcript, composer.
class ChatPane extends ConsumerWidget {
  const ChatPane({
    super.key,
    this.onOpenSessions,
    this.onOpenMemory,
    this.onOpenSettings,
  });

  final VoidCallback? onOpenSessions;
  final VoidCallback? onOpenMemory;
  final VoidCallback? onOpenSettings;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final controller = ref.read(chatControllerProvider.notifier);
    final sessionId = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    final hasSession = sessionId != null;
    final title = ref.watch(
      chatControllerProvider.select((s) => s.activeSession?.title),
    );
    final streaming = ref.watch(
      chatControllerProvider.select((s) => s.isStreamingActive),
    );

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: title ?? 'Cortex',
          subtitle: hasSession ? null : '记忆原生的通用 AI Agent',
          leading: onOpenSessions == null
              ? null
              : IconButton(
                  onPressed: onOpenSessions,
                  iconSize: 19,
                  tooltip: '会话列表',
                  icon: const Icon(Icons.menu_rounded),
                ),
          actions: [
            // First in the row on purpose: whether the agent can touch files is
            // the single most consequential property of a session, and it must
            // be answerable without opening anything.
            if (hasSession) ...[
              const WorkspaceChip(),
              const SizedBox(width: 8),
            ],
            const SyncIndicator(),
            const _BackendBadge(),
            if (onOpenSettings != null)
              IconButton(
                onPressed: onOpenSettings,
                iconSize: 18,
                tooltip: '设置',
                icon: const Icon(Icons.tune_rounded),
              ),
            IconButton(
              onPressed: () => ref.read(themeModeProvider.notifier).cycle(),
              iconSize: 18,
              tooltip: '切换主题',
              icon: Icon(
                switch (ref.watch(themeModeProvider)) {
                  ThemeMode.system => Icons.brightness_auto_rounded,
                  ThemeMode.light => Icons.light_mode_rounded,
                  ThemeMode.dark => Icons.dark_mode_rounded,
                },
              ),
            ),
            if (onOpenMemory != null)
              IconButton(
                onPressed: onOpenMemory,
                iconSize: 19,
                tooltip: '记忆面板',
                icon: const Icon(Icons.psychology_outlined),
              ),
            // 最后一个：平时是「关于」，有新版本时右上角多一个点。
            // 放在末尾是因为它是这一行里唯一**多数时候没事**的那个
            const UpdateIndicator(),
          ],
        ),
        Expanded(
          child: hasSession
              ? const ConversationView()
              : NoSessionState(onCreate: controller.createSession),
        ),
        const _OfflineBanner(),
        const _SendErrorBanner(),
        // Directly above the composer: the one place the user's eyes already
        // are when a turn is running, and close enough to the send button that
        // it cannot be scrolled past. See `ConfirmPanel` for why it is not a
        // modal.
        const ConfirmPanel(),
        MessageComposer(
          enabled: hasSession,
          sessionId: sessionId,
          streaming: streaming,
          onSend: (text, attachments) =>
              controller.send(text, attachments: attachments),
          onStop: controller.stopGeneration,
        ),
      ],
    );
  }
}

/// Tells the user at a glance whether they are looking at real data.
class _BackendBadge extends ConsumerWidget {
  const _BackendBadge();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final config = ref.watch(appConfigProvider);
    final health = ref.watch(healthProvider);

    final (Color color, String label, String tooltip) = switch ((
      config.useMock,
      health,
    )) {
      (true, _) => (
        scheme.onSurfaceVariant,
        'MOCK',
        '数据来自内存夹具，未连接后端',
      ),
      (false, AsyncData(:final value)) when value.isHealthy => (
        const Color(0xFF2E9E5B),
        'LIVE',
        [
          config.baseUrl,
          'v${value.version}',
          if (value.databaseNote != null) value.databaseNote!,
        ].join(' · '),
      ),
      (false, AsyncError()) => (
        scheme.error,
        'DOWN',
        '连不上 ${config.baseUrl}',
      ),
      _ => (scheme.onSurfaceVariant, '…', '正在检测 ${config.baseUrl}'),
    };

    return Tooltip(
      message: tooltip,
      child: Container(
        margin: const EdgeInsets.symmetric(horizontal: 4),
        padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 3),
        decoration: BoxDecoration(
          color: color.withValues(alpha: 0.12),
          borderRadius: BorderRadius.circular(5),
          border: Border.all(color: color.withValues(alpha: 0.35)),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Container(
              width: 5,
              height: 5,
              decoration: BoxDecoration(color: color, shape: BoxShape.circle),
            ),
            const SizedBox(width: 5),
            Text(
              label,
              style: TextStyle(
                fontSize: 10,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.6,
                color: color,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// 离线模式期间**一直**挂着的一条提示。
///
/// # 为什么不是「入口说一次就够了」
///
/// 这个模式唯一的代价是「没有记忆」，而记忆恰恰是这个产品的全部卖点。
/// 一个在入口点了「离线使用」的人，二十分钟后正在专心干活时，
/// 完全可能忘了自己处在一个不记事的状态里 —— 然后对着一句
/// 「我上次说的那个方案」发现它什么都不知道。
///
/// 所以这条不可关闭：它不是通知，是**当前状态**。
class _OfflineBanner extends ConsumerWidget {
  const _OfflineBanner();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final offline = ref.watch(appConfigProvider.select((c) => c.offline));
    if (!offline) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      width: double.infinity,
      color: scheme.tertiaryContainer,
      padding: const EdgeInsets.fromLTRB(20, 8, 10, 8),
      child: Row(
        children: [
          Icon(
            Icons.cloud_off_outlined,
            size: 16,
            color: scheme.onTertiaryContainer,
          ),
          const SizedBox(width: 9),
          Expanded(
            // 同上：Text 不渲染 Markdown，星号会原样显示
            child: Text.rich(
              TextSpan(
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onTertiaryContainer,
                ),
                children: const [
                  TextSpan(text: '离线模式：这些对话'),
                  TextSpan(
                    text: '没有在记忆里',
                    style: TextStyle(fontWeight: FontWeight.w700),
                  ),
                  TextSpan(text: '。它们排在本地队列，接上服务器后会自动补回去。'),
                ],
              ),
            ),
          ),
          TextButton(
            onPressed: () =>
                ref.read(appConfigProvider.notifier).setOffline(false),
            child: const Text('去连接'),
          ),
        ],
      ),
    );
  }
}

class _SendErrorBanner extends ConsumerWidget {
  const _SendErrorBanner();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final error = ref.watch(
      chatControllerProvider.select((s) => s.sendError),
    );
    if (error == null) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final controller = ref.read(chatControllerProvider.notifier);

    return Container(
      width: double.infinity,
      color: scheme.errorContainer,
      padding: const EdgeInsets.fromLTRB(20, 10, 10, 10),
      child: Row(
        children: [
          Icon(Icons.error_outline_rounded, size: 16, color: scheme.error),
          const SizedBox(width: 9),
          Expanded(
            child: Text(
              error,
              maxLines: 3,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onErrorContainer,
              ),
            ),
          ),
          TextButton(
            onPressed: controller.retryLast,
            child: const Text('重试'),
          ),
          IconButton(
            onPressed: controller.clearSendError,
            iconSize: 16,
            icon: const Icon(Icons.close_rounded),
          ),
        ],
      ),
    );
  }
}
