import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/formatting.dart';
import '../../../core/theme.dart';
import '../../../models/episode.dart';
import '../../../state/memory_controller.dart';
import '../../../widgets/empty_state.dart';

/// Opens the provenance view for a fact: the archived episode it came from.
///
/// This is the payoff of the append-only design — every fact can answer "where
/// did you get that?" with the original turn, verbatim.
Future<void> showEpisodeSheet(BuildContext context, String episodeId) {
  return showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    showDragHandle: true,
    backgroundColor: Theme.of(context).colorScheme.surfaceContainerLow,
    constraints: const BoxConstraints(maxWidth: 720),
    builder: (_) => _EpisodeSheet(episodeId: episodeId),
  );
}

class _EpisodeSheet extends ConsumerWidget {
  const _EpisodeSheet({required this.episodeId});

  final String episodeId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final async = ref.watch(episodeProvider(episodeId));

    return SafeArea(
      top: false,
      child: ConstrainedBox(
        constraints: BoxConstraints(
          maxHeight: MediaQuery.sizeOf(context).height * 0.72,
          minHeight: 220,
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 0, 20, 12),
              child: Row(
                children: [
                  Icon(
                    Icons.history_edu_outlined,
                    size: 18,
                    color: scheme.secondary,
                  ),
                  const SizedBox(width: 8),
                  Text('记忆出处', style: theme.textTheme.titleMedium),
                  const Spacer(),
                  SelectableText(
                    episodeId,
                    style: theme.textTheme.labelSmall?.copyWith(
                      fontFamily: 'monospace',
                      fontFamilyFallback: CortexTheme.monoFallback,
                    ),
                  ),
                ],
              ),
            ),
            const Divider(height: 1),
            Flexible(
              child: async.when(
                loading: () => const Padding(
                  padding: EdgeInsets.symmetric(vertical: 48),
                  child: Center(
                    child: SizedBox(
                      width: 22,
                      height: 22,
                      child: CircularProgressIndicator(strokeWidth: 2.2),
                    ),
                  ),
                ),
                error: (e, _) => EmptyState(
                  icon: Icons.link_off_rounded,
                  title: '取不到这条出处',
                  description: '$e',
                  tone: EmptyStateTone.error,
                ),
                data: (episode) => _EpisodeBody(episode: episode),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _EpisodeBody extends StatelessWidget {
  const _EpisodeBody({required this.episode});

  final Episode episode;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final isUser = episode.role == 'user';

    return SingleChildScrollView(
      padding: const EdgeInsets.fromLTRB(20, 16, 20, 24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Wrap(
            spacing: 8,
            runSpacing: 8,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              _Pill(
                icon: isUser
                    ? Icons.person_outline_rounded
                    : Icons.auto_awesome_outlined,
                label: isUser ? '用户' : episode.role,
                color: isUser ? scheme.primary : scheme.secondary,
              ),
              _Pill(
                icon: Icons.schedule_rounded,
                label: formatAbsolute(episode.occurredAt),
                color: scheme.onSurfaceVariant,
              ),
              _Pill(
                icon: Icons.forum_outlined,
                label: '会话 ${shortId(episode.sessionId)}',
                color: scheme.onSurfaceVariant,
              ),
            ],
          ),
          const SizedBox(height: 16),
          Container(
            width: double.infinity,
            padding: const EdgeInsets.all(16),
            decoration: BoxDecoration(
              color: scheme.surfaceContainer,
              borderRadius: BorderRadius.circular(12),
              border: Border(
                left: BorderSide(
                  color: isUser ? scheme.primary : scheme.secondary,
                  width: 3,
                ),
                top: BorderSide(color: scheme.outlineVariant),
                right: BorderSide(color: scheme.outlineVariant),
                bottom: BorderSide(color: scheme.outlineVariant),
              ),
            ),
            child: SelectableText(
              episode.text,
              style: theme.textTheme.bodyMedium?.copyWith(height: 1.7),
            ),
          ),
          const SizedBox(height: 12),
          Text(
            '归档记录不可变更 —— 这段文本与当时写入时逐字一致。',
            style: theme.textTheme.labelSmall,
          ),
        ],
      ),
    );
  }
}

class _Pill extends StatelessWidget {
  const _Pill({required this.icon, required this.label, required this.color});

  final IconData icon;
  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 9, vertical: 5),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.10),
        borderRadius: BorderRadius.circular(20),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 13, color: color),
          const SizedBox(width: 5),
          Text(
            label,
            style: theme.textTheme.labelSmall?.copyWith(color: color),
          ),
        ],
      ),
    );
  }
}
