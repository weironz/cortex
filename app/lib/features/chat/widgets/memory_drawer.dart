import 'package:flutter/material.dart';

import '../../../models/memory_fact.dart';
import '../../../models/tool_call.dart';
import '../../memory/widgets/fact_card.dart';

/// "本轮用到的记忆" — the per-turn audit affordance.
///
/// Collapsed by default: it must be available on every assistant turn without
/// competing with the answer itself. Expanding lists the injected facts, each
/// of which opens its source episode.
class MemoryDrawer extends StatefulWidget {
  const MemoryDrawer({
    super.key,
    required this.facts,
    this.toolCalls = const [],
    this.initiallyExpanded = false,
  });

  final List<MemoryFact> facts;
  final List<ToolCall> toolCalls;
  final bool initiallyExpanded;

  @override
  State<MemoryDrawer> createState() => _MemoryDrawerState();
}

class _MemoryDrawerState extends State<MemoryDrawer> {
  late bool _expanded = widget.initiallyExpanded;

  @override
  Widget build(BuildContext context) {
    if (widget.facts.isEmpty && widget.toolCalls.isEmpty) {
      return const SizedBox.shrink();
    }

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Padding(
      padding: const EdgeInsets.only(top: 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _Toggle(
            expanded: _expanded,
            factCount: widget.facts.length,
            toolCount: widget.toolCalls.length,
            onTap: () => setState(() => _expanded = !_expanded),
          ),
          AnimatedSize(
            duration: const Duration(milliseconds: 160),
            curve: Curves.easeOutCubic,
            alignment: Alignment.topLeft,
            child: _expanded
                ? Container(
                    width: double.infinity,
                    margin: const EdgeInsets.only(top: 8),
                    padding: const EdgeInsets.fromLTRB(12, 12, 12, 6),
                    decoration: BoxDecoration(
                      borderRadius: BorderRadius.circular(11),
                      border: Border.all(
                        color: scheme.secondary.withValues(alpha: 0.28),
                      ),
                      color: scheme.secondary.withValues(alpha: 0.04),
                    ),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        if (widget.toolCalls.isNotEmpty) ...[
                          for (final call in widget.toolCalls)
                            _ToolRow(call: call),
                          if (widget.facts.isNotEmpty)
                            Padding(
                              padding: const EdgeInsets.only(
                                top: 4,
                                bottom: 10,
                              ),
                              child: Divider(
                                height: 1,
                                color: scheme.outlineVariant,
                              ),
                            ),
                        ],
                        if (widget.facts.isNotEmpty) ...[
                          Padding(
                            padding: const EdgeInsets.only(bottom: 8),
                            child: Text(
                              '以下事实作为背景数据注入了本轮提示词，点任意一条可查看原始出处。',
                              style: theme.textTheme.labelSmall,
                            ),
                          ),
                          for (final fact in widget.facts)
                            FactCard(fact: fact, dense: true),
                        ],
                      ],
                    ),
                  )
                : const SizedBox(width: double.infinity),
          ),
        ],
      ),
    );
  }
}

class _Toggle extends StatelessWidget {
  const _Toggle({
    required this.expanded,
    required this.factCount,
    required this.toolCount,
    required this.onTap,
  });

  final bool expanded;
  final int factCount;
  final int toolCount;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    final label = factCount > 0
        ? '本轮用到的记忆 · $factCount 条'
        : '本轮工具调用 · $toolCount 次';

    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(7),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 4),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.psychology_outlined, size: 14, color: scheme.secondary),
            const SizedBox(width: 6),
            Text(
              label,
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.secondary,
                fontWeight: FontWeight.w600,
              ),
            ),
            const SizedBox(width: 2),
            AnimatedRotation(
              turns: expanded ? 0.5 : 0,
              duration: const Duration(milliseconds: 160),
              child: Icon(
                Icons.expand_more_rounded,
                size: 15,
                color: scheme.secondary,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _ToolRow extends StatelessWidget {
  const _ToolRow({required this.call});

  final ToolCall call;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(
            Icons.terminal_rounded,
            size: 13,
            color: scheme.onSurfaceVariant,
          ),
          const SizedBox(width: 7),
          Expanded(
            child: RichText(
              text: TextSpan(
                style: theme.textTheme.labelSmall,
                children: [
                  TextSpan(
                    text: call.name,
                    style: TextStyle(
                      fontFamily: 'monospace',
                      fontWeight: FontWeight.w600,
                      color: scheme.onSurface,
                    ),
                  ),
                  if (call.summary != null)
                    TextSpan(text: '  ${call.summary}'),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}
