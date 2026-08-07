import 'package:flutter/material.dart';

import '../../../models/memory_fact.dart';
import '../../../models/tool_call.dart';
import '../../memory/widgets/fact_card.dart';

/// The per-turn audit affordance: what was injected, and what the agent did.
///
/// ## Two different jobs, two different treatments
///
/// * **While the turn is streaming**, tool rows are shown inline and
///   unconditionally. A `read_file` that takes two seconds is the answer to
///   "why is nothing happening", and burying it one click deep would waste it.
/// * **Once the turn is committed**, everything collapses behind one thin line.
///   The audit trail must be available on every answer without competing with
///   the answer itself.
///
/// ## Nothing here assumes memory exists
///
/// The retriever abstains when a question is unrelated to anything stored, so
/// `facts` is legitimately empty on plenty of turns. That is a correct outcome,
/// not a failure, and it is rendered as such — no error styling, no "加载失败",
/// and no toggle at all when there is also no tool activity to show.
class MemoryDrawer extends StatefulWidget {
  const MemoryDrawer({
    super.key,
    required this.facts,
    this.toolCalls = const [],
    this.streaming = false,
    this.initiallyExpanded = false,
  });

  final List<MemoryFact> facts;
  final List<ToolCall> toolCalls;

  /// The turn is still in flight — surfaces tool activity instead of hiding it.
  final bool streaming;

  final bool initiallyExpanded;

  @override
  State<MemoryDrawer> createState() => _MemoryDrawerState();
}

class _MemoryDrawerState extends State<MemoryDrawer> {
  late bool _expanded = widget.initiallyExpanded;

  @override
  Widget build(BuildContext context) {
    final hasFacts = widget.facts.isNotEmpty;
    final hasTools = widget.toolCalls.isNotEmpty;
    if (!hasFacts && !hasTools) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    // Live turn: tools stay visible, facts stay one click away.
    if (widget.streaming) {
      return Padding(
        padding: const EdgeInsets.only(top: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            for (final call in widget.toolCalls) _ToolRow(call: call),
            if (hasFacts)
              _Toggle(
                expanded: _expanded,
                label: '本轮用到的记忆 · ${widget.facts.length} 条',
                onTap: () => setState(() => _expanded = !_expanded),
              ),
            if (hasFacts && _expanded)
              _Panel(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const _InjectionNote(),
                    for (final fact in widget.facts)
                      FactCard(fact: fact, dense: true),
                  ],
                ),
              ),
          ],
        ),
      );
    }

    return Padding(
      padding: const EdgeInsets.only(top: 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _Toggle(
            expanded: _expanded,
            label: _label(widget.facts.length, widget.toolCalls.length),
            onTap: () => setState(() => _expanded = !_expanded),
          ),
          AnimatedSize(
            duration: const Duration(milliseconds: 160),
            curve: Curves.easeOutCubic,
            alignment: Alignment.topLeft,
            child: _expanded
                ? _Panel(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        if (hasTools) ...[
                          for (final call in widget.toolCalls)
                            _ToolRow(call: call),
                          if (hasFacts)
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
                        if (hasFacts) ...[
                          const _InjectionNote(),
                          for (final fact in widget.facts)
                            FactCard(fact: fact, dense: true),
                        ] else
                          // Explicitly stated rather than left blank: an empty
                          // panel reads as a bug, and this outcome is neither
                          // rare nor wrong.
                          Padding(
                            padding: const EdgeInsets.only(bottom: 6),
                            child: Text(
                              '本轮没有注入任何记忆 —— 检索器判断这个问题与已存记忆无关，'
                              '于是主动弃权。这是正常结果。',
                              style: theme.textTheme.labelSmall,
                            ),
                          ),
                      ],
                    ),
                  )
                : const SizedBox(width: double.infinity),
          ),
        ],
      ),
    );
  }

  static String _label(int facts, int tools) {
    if (facts > 0 && tools > 0) return '本轮用到的记忆 · $facts 条 · 工具 $tools 次';
    if (facts > 0) return '本轮用到的记忆 · $facts 条';
    return '本轮工具调用 · $tools 次';
  }
}

class _Panel extends StatelessWidget {
  const _Panel({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.only(top: 8),
      padding: const EdgeInsets.fromLTRB(12, 12, 12, 6),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(11),
        border: Border.all(color: scheme.secondary.withValues(alpha: 0.28)),
        color: scheme.secondary.withValues(alpha: 0.04),
      ),
      child: child,
    );
  }
}

class _InjectionNote extends StatelessWidget {
  const _InjectionNote();

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: 8),
    child: Text(
      '以下事实作为背景数据注入了本轮提示词，点任意一条可查看原始出处。',
      style: Theme.of(context).textTheme.labelSmall,
    ),
  );
}

class _Toggle extends StatelessWidget {
  const _Toggle({
    required this.expanded,
    required this.label,
    required this.onTap,
  });

  final bool expanded;
  final String label;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

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

/// One invocation — call and result on a single line.
class _ToolRow extends StatelessWidget {
  const _ToolRow({required this.call});

  final ToolCall call;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final labelStyle = theme.textTheme.labelSmall ?? const TextStyle();

    final Color icon;
    if (call.failed) {
      icon = scheme.error;
    } else if (call.pending) {
      icon = scheme.secondary;
    } else {
      icon = scheme.onSurfaceVariant;
    }

    final path = call.targetPath;

    return Padding(
      padding: const EdgeInsets.only(left: 7, bottom: 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(top: 1),
            child: Icon(
              call.failed
                  ? Icons.error_outline_rounded
                  // A file tool gets a file icon: at a glance the row says
                  // "this touched your disk" rather than "something ran".
                  : call.touchesFiles
                  ? Icons.description_outlined
                  : Icons.terminal_rounded,
              size: 13,
              color: icon,
            ),
          ),
          const SizedBox(width: 7),
          Expanded(
            child: RichText(
              text: TextSpan(
                style: labelStyle,
                children: [
                  TextSpan(
                    text: call.name,
                    style: labelStyle.copyWith(
                      fontFamily: 'monospace',
                      fontWeight: FontWeight.w600,
                      color: scheme.onSurface,
                    ),
                  ),
                  // The file a file-tool touched is pulled out of the argument
                  // blob and given the emphasis the rest of the arguments do
                  // not get. "write_file 改了哪个文件" is the one question a
                  // reader always has about these rows, and leaving it buried
                  // mid-string next to a truncated `content=` makes it
                  // effectively invisible.
                  if (path != null)
                    TextSpan(
                      text: '  $path',
                      style: labelStyle.copyWith(
                        fontFamily: 'monospace',
                        color: scheme.secondary,
                      ),
                    )
                  else if (call.arguments != null)
                    TextSpan(text: '  ${call.arguments}'),
                  if (call.result != null)
                    TextSpan(
                      text: '  · ${call.result}',
                      style: labelStyle.copyWith(
                        color: call.failed
                            ? scheme.error
                            : scheme.onSurfaceVariant,
                      ),
                    ),
                ],
              ),
            ),
          ),
          if (call.pending) ...[
            const SizedBox(width: 8),
            Padding(
              padding: const EdgeInsets.only(top: 2),
              child: SizedBox(
                width: 9,
                height: 9,
                child: CircularProgressIndicator(
                  strokeWidth: 1.4,
                  color: scheme.secondary,
                ),
              ),
            ),
          ],
        ],
      ),
    );
  }
}
