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
    this.invalidated = false,
  });

  final MemoryFact fact;
  final RetrievalChannels? channels;

  /// Compact variant used inside a chat message's "memory used" drawer.
  final bool dense;

  /// The fact has been superseded since it was injected.
  ///
  /// Only the replay path knows this. Marked rather than filtered: the point of
  /// the drawer is to show what the answer was actually built on, and "that is
  /// no longer true" is the most useful thing it can tell you about an old
  /// answer. The statement itself stays fully legible — greying it out would
  /// make an audit record harder to read for no gain.
  final bool invalidated;

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
    final hasTime = fact.validAt != null || fact.createdAt != null;

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
                  if (widget.invalidated)
                    _Tag(
                      label: '已失效',
                      color: scheme.error,
                      filled: true,
                    ),
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
                      color: _channelColor(c, scheme),
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
                  // A replayed injection carries only the statement and the
                  // domain — `InjectedMemoryDto` has no timestamps. Printing
                  // "生效 —" for those would be noise dressed up as data, so the
                  // whole clause drops out and only the provenance link stays.
                  if (hasTime) ...[
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
                  ] else
                    const Spacer(),
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

  /// One colour per retrieval channel.
  ///
  /// The live daemon fuses four to five channels (`bm25`, `vector`, `graph`,
  /// `recency`, `episode`), so painting them all the same made the row read as
  /// undifferentiated noise. Unknown names fall back to the neutral tone rather
  /// than being dropped — a new channel should show up, not disappear.
  static Color _channelColor(String channel, ColorScheme scheme) =>
      switch (channel) {
        'bm25' => const Color(0xFFB07219),
        'vector' => scheme.primary,
        'graph' => const Color(0xFF7A5FBF),
        'recency' => const Color(0xFF2E8B8B),
        'episode' => const Color(0xFF3F7DBF),
        _ => scheme.onSurfaceVariant,
      };

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
