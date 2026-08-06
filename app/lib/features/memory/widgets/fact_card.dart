import 'package:flutter/material.dart';

import '../../../core/formatting.dart';
import '../../../models/memory_fact.dart';
import '../../../models/memory_search_result.dart';
import 'episode_sheet.dart';

/// One retrieved fact.
///
/// Shows the statement first (it is the only thing worth reading at a glance),
/// then the audit row: domain, confidence, valid time, retrieval channels, and
/// a jump to provenance.
class FactCard extends StatefulWidget {
  const FactCard({
    super.key,
    required this.fact,
    this.channels,
    this.dense = false,
  });

  final MemoryFact fact;
  final RetrievalChannels? channels;

  /// Compact variant used inside a chat message's "memory used" drawer.
  final bool dense;

  @override
  State<FactCard> createState() => _FactCardState();
}

class _FactCardState extends State<FactCard> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final fact = widget.fact;
    final episodeId = fact.sourceEpisodeId;
    final canOpen = episodeId != null && episodeId.isNotEmpty;

    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      cursor: canOpen ? SystemMouseCursors.click : MouseCursor.defer,
      child: GestureDetector(
        onTap: canOpen ? () => showEpisodeSheet(context, episodeId) : null,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 120),
          margin: EdgeInsets.only(bottom: widget.dense ? 6 : 8),
          padding: EdgeInsets.all(widget.dense ? 10 : 13),
          decoration: BoxDecoration(
            color: _hovered && canOpen
                ? scheme.surfaceContainerHigh
                : scheme.surfaceContainerLow,
            borderRadius: BorderRadius.circular(10),
            border: Border.all(
              color: _hovered && canOpen
                  ? scheme.secondary.withValues(alpha: 0.45)
                  : scheme.outlineVariant,
            ),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                fact.statement,
                style: (widget.dense
                        ? theme.textTheme.bodySmall
                        : theme.textTheme.bodyMedium)
                    ?.copyWith(color: scheme.onSurface, height: 1.55),
              ),
              SizedBox(height: widget.dense ? 7 : 9),
              Wrap(
                spacing: 6,
                runSpacing: 6,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  if (fact.domain != null)
                    _Tag(
                      label: fact.domain!,
                      color: scheme.secondary,
                      filled: true,
                    ),
                  if (fact.predicate != null)
                    _Tag(
                      label: fact.predicate!,
                      color: scheme.onSurfaceVariant,
                      mono: true,
                    ),
                  ...?widget.channels?.channels.map(
                    (c) => _Tag(
                      label: c,
                      color: c == 'bm25'
                          ? const Color(0xFFB07219)
                          : scheme.primary,
                      mono: true,
                    ),
                  ),
                  if (fact.confidence != null)
                    _Tag(
                      label: formatConfidence(fact.confidence),
                      color: _confidenceColor(fact.confidence!, scheme),
                    ),
                ],
              ),
              SizedBox(height: widget.dense ? 6 : 8),
              Row(
                children: [
                  Icon(
                    Icons.schedule_rounded,
                    size: 12,
                    color: scheme.onSurfaceVariant,
                  ),
                  const SizedBox(width: 4),
                  Flexible(
                    child: Text(
                      '生效 ${formatDate(fact.validAt)}'
                      '${fact.createdAt != null ? ' · 记录 ${formatDate(fact.createdAt)}' : ''}',
                      style: theme.textTheme.labelSmall,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ),
                  if (canOpen) ...[
                    const SizedBox(width: 8),
                    Text(
                      '出处',
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: scheme.secondary,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    Icon(
                      Icons.chevron_right_rounded,
                      size: 14,
                      color: scheme.secondary,
                    ),
                  ],
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  static Color _confidenceColor(double c, ColorScheme scheme) {
    if (c >= 0.9) return const Color(0xFF2E7D32);
    if (c >= 0.75) return scheme.onSurfaceVariant;
    return const Color(0xFFB26A00);
  }
}

class _Tag extends StatelessWidget {
  const _Tag({
    required this.label,
    required this.color,
    this.filled = false,
    this.mono = false,
  });

  final String label;
  final Color color;
  final bool filled;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: filled
            ? color.withValues(alpha: 0.12)
            : Theme.of(context).colorScheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(5),
        border: Border.all(color: color.withValues(alpha: 0.28)),
      ),
      child: Text(
        label,
        style: TextStyle(
          fontSize: 10.5,
          height: 1.3,
          color: color,
          fontWeight: FontWeight.w600,
          fontFamily: mono ? 'monospace' : null,
        ),
      ),
    );
  }
}
