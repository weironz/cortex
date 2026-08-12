import 'diff_view.dart';
import 'package:flutter/material.dart';

import '../../../models/injected_memory.dart';
import '../../../models/memory_search_result.dart';
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
///
/// ## Replayed turns show the same drawer
///
/// `episode_memories` / `episode_tool_calls` mean a turn reopened tomorrow
/// still answers "why do you remember that". Two things only a replay can say
/// are surfaced rather than smoothed over: which retrieval channels matched,
/// and whether a fact has since been **invalidated**. A superseded fact stays
/// in the list, marked — the answer really was built on it, and hiding that
/// would be rewriting history rather than recording it.
class MemoryDrawer extends StatefulWidget {
  const MemoryDrawer({
    super.key,
    required this.facts,
    this.toolCalls = const [],
    this.streaming = false,
    this.initiallyExpanded = false,
  });

  final List<InjectedMemory> facts;
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
                    for (final entry in widget.facts)
                      _InjectedRow(entry: entry),
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
                          for (final entry in widget.facts)
                            _InjectedRow(entry: entry),
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

/// One injected fact: the normal card, or a placeholder when the row is gone.
class _InjectedRow extends StatelessWidget {
  const _InjectedRow({required this.entry});

  final InjectedMemory entry;

  @override
  Widget build(BuildContext context) {
    final fact = entry.fact;
    if (fact == null) return _RedactedNote(factId: entry.factId);
    return FactCard(
      fact: fact,
      dense: true,
      invalidated: entry.invalidated,
      // Live turns carry no attribution, and an empty channel list must render
      // as *no* tags rather than as an empty row of them.
      channels: entry.channels.isEmpty
          ? null
          : RetrievalChannels(
              factId: entry.factId,
              channels: entry.channels,
              score: entry.score,
            ),
    );
  }
}

/// The fact this turn used has since been redacted or purged.
///
/// Shown rather than dropped. Silently omitting the entry would make the turn
/// look like it consulted fewer memories than it did — a replay that quietly
/// disagrees with what happened is worse than one that admits a gap.
class _RedactedNote extends StatelessWidget {
  const _RedactedNote({required this.factId});

  final String factId;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.only(bottom: 6),
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerLow,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(
          color: scheme.outlineVariant,
          style: BorderStyle.solid,
        ),
      ),
      child: Row(
        children: [
          Icon(
            Icons.visibility_off_outlined,
            size: 13,
            color: scheme.onSurfaceVariant,
          ),
          const SizedBox(width: 7),
          Expanded(
            child: Text(
              '本轮引用了一条已不可见的记忆（$factId）—— 它在此之后被抹除了。',
              style: theme.textTheme.labelSmall,
            ),
          ),
        ],
      ),
    );
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
/// 一次工具调用。有 diff 的话可以展开看这次改了什么。
///
/// 默认**收着**：一轮里可能有五六次写入，全部摊开会把这一段变成一片
/// 看不完的绿红，而抽屉本来是「扫一眼这轮干了什么」用的。
class _ToolRow extends StatefulWidget {
  const _ToolRow({required this.call});

  final ToolCall call;

  @override
  State<_ToolRow> createState() => _ToolRowState();
}

class _ToolRowState extends State<_ToolRow> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final call = widget.call;
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

    final path = call.path;

    final row = Padding(
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
                  // The file a file-tool touched gets the emphasis the rest of
                  // the arguments do not. "write_file 改了哪个文件" is the one
                  // question a reader always has about these rows, and leaving
                  // it buried mid-string next to a truncated `content=` makes
                  // it effectively invisible.
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
          // 有改动才给展开箭头 —— 一个点下去什么都不展开的箭头，
          // 比没有箭头更让人困惑
          if (call.diff != null)
            InkResponse(
              onTap: () => setState(() => _open = !_open),
              radius: 12,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 4),
                child: Icon(
                  _open
                      ? Icons.keyboard_arrow_down_rounded
                      : Icons.keyboard_arrow_right_rounded,
                  size: 15,
                  color: scheme.onSurfaceVariant,
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

    if (!_open || call.diff == null) return row;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        row,
        Padding(
          padding: const EdgeInsets.only(left: 27, bottom: 8, right: 4),
          child: DiffView(call.diff!),
        ),
      ],
    );
  }
}
