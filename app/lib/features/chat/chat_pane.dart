import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../../widgets/panel_header.dart';
import '../shell/widgets/sync_indicator.dart';
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
          ],
        ),
        Expanded(
          child: hasSession
              ? const ConversationView()
              : NoSessionState(onCreate: controller.createSession),
        ),
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
