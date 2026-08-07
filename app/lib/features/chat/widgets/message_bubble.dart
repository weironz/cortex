import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../../core/formatting.dart';
import '../../../core/link_launcher.dart';
import '../../../models/chat_message.dart';
import '../../../models/memory_fact.dart';
import '../../../models/tool_call.dart';
import '../../../widgets/markdown/cortex_markdown.dart';
import 'memory_drawer.dart';

/// Horizontal padding around the conversation column.
const kMessageGutter = 28.0;

/// Upper bound on line length. Long measure hurts readability badly in CJK,
/// and an ultrawide monitor would otherwise stretch a paragraph across 2000px.
const kMessageMaxWidth = 760.0;

/// A committed message.
///
/// Assistant text goes through [CortexMarkdown]; user text stays plain — echoing
/// the user's own input through a markdown parser would silently mangle any
/// `*` or `#` they typed literally.
class MessageBubble extends StatelessWidget {
  const MessageBubble({super.key, required this.message});

  final ChatMessage message;

  @override
  Widget build(BuildContext context) {
    return message.role == MessageRole.user
        ? _UserBubble(message: message)
        : AssistantBlock(
            text: message.text,
            facts: message.facts,
            toolCalls: message.toolCalls,
            createdAt: message.createdAt,
            episodeId: message.episodeId,
            error: message.error,
          );
  }
}

class _UserBubble extends StatelessWidget {
  const _UserBubble({required this.message});

  final ChatMessage message;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Padding(
      padding: const EdgeInsets.fromLTRB(
        kMessageGutter,
        12,
        kMessageGutter,
        12,
      ),
      child: Align(
        alignment: Alignment.centerRight,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: kMessageMaxWidth * 0.82),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.end,
            children: [
              Container(
                padding: const EdgeInsets.symmetric(
                  horizontal: 15,
                  vertical: 11,
                ),
                decoration: BoxDecoration(
                  color: scheme.primary,
                  borderRadius: const BorderRadius.only(
                    topLeft: Radius.circular(14),
                    topRight: Radius.circular(14),
                    bottomLeft: Radius.circular(14),
                    bottomRight: Radius.circular(4),
                  ),
                ),
                child: SelectionArea(
                  child: Text(
                    message.text,
                    style: theme.textTheme.bodyLarge?.copyWith(
                      color: scheme.onPrimary,
                    ),
                  ),
                ),
              ),
              const SizedBox(height: 4),
              Text(
                formatRelative(message.createdAt),
                style: theme.textTheme.labelSmall,
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Assistant side. Also used for the live streaming turn, which is why it takes
/// raw fields rather than a [ChatMessage].
class AssistantBlock extends StatelessWidget {
  const AssistantBlock({
    super.key,
    required this.text,
    required this.facts,
    required this.toolCalls,
    this.createdAt,
    this.episodeId,
    this.error,
    this.streaming = false,
  });

  final String text;
  final List<MemoryFact> facts;
  final List<ToolCall> toolCalls;
  final DateTime? createdAt;
  final String? episodeId;
  final String? error;
  final bool streaming;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Padding(
      padding: const EdgeInsets.fromLTRB(
        kMessageGutter,
        12,
        kMessageGutter,
        12,
      ),
      child: Align(
        alignment: Alignment.centerLeft,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: kMessageMaxWidth),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Container(
                    width: 21,
                    height: 21,
                    decoration: BoxDecoration(
                      gradient: LinearGradient(
                        colors: [
                          scheme.primary,
                          scheme.secondary.withValues(alpha: 0.9),
                        ],
                      ),
                      borderRadius: BorderRadius.circular(6),
                    ),
                    child: const Icon(
                      Icons.auto_awesome,
                      size: 12,
                      color: Colors.white,
                    ),
                  ),
                  const SizedBox(width: 8),
                  Text('Cortex', style: theme.textTheme.titleSmall),
                  if (createdAt != null && !streaming) ...[
                    const SizedBox(width: 8),
                    Text(
                      formatRelative(createdAt),
                      style: theme.textTheme.labelSmall,
                    ),
                  ],
                  const Spacer(),
                  if (!streaming && text.isNotEmpty)
                    _CopyButton(text: text),
                ],
              ),
              const SizedBox(height: 6),
              Padding(
                padding: const EdgeInsets.only(left: 29),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (text.isNotEmpty)
                      SelectionArea(
                        child: CortexMarkdown(
                          text,
                          onLinkTap: (url) => openExternalLink(context, url),
                        ),
                      ),
                    if (streaming) ...[
                      if (text.isEmpty)
                        const _ThinkingIndicator()
                      else
                        const _Caret(),
                    ],
                    if (error != null) _ErrorNote(error: error!),
                    MemoryDrawer(
                      facts: facts,
                      toolCalls: toolCalls,
                      streaming: streaming,
                    ),
                    if (episodeId != null && !streaming)
                      Padding(
                        padding: const EdgeInsets.only(top: 6, left: 7),
                        child: Text(
                          'episode ${shortId(episodeId)}',
                          style: theme.textTheme.labelSmall?.copyWith(
                            fontFamily: 'monospace',
                          ),
                        ),
                      ),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _CopyButton extends StatefulWidget {
  const _CopyButton({required this.text});
  final String text;

  @override
  State<_CopyButton> createState() => _CopyButtonState();
}

class _CopyButtonState extends State<_CopyButton> {
  bool _done = false;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return IconButton(
      iconSize: 14,
      visualDensity: VisualDensity.compact,
      padding: const EdgeInsets.all(5),
      constraints: const BoxConstraints(),
      tooltip: _done ? '已复制' : '复制回答',
      onPressed: () async {
        await Clipboard.setData(ClipboardData(text: widget.text));
        if (!mounted) return;
        setState(() => _done = true);
        await Future<void>.delayed(const Duration(milliseconds: 1500));
        if (mounted) setState(() => _done = false);
      },
      icon: Icon(
        _done ? Icons.check_rounded : Icons.copy_rounded,
        color: _done ? scheme.secondary : scheme.onSurfaceVariant,
      ),
    );
  }
}

class _ErrorNote extends StatelessWidget {
  const _ErrorNote({required this.error});
  final String error;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      margin: const EdgeInsets.only(top: 8),
      padding: const EdgeInsets.symmetric(horizontal: 11, vertical: 8),
      decoration: BoxDecoration(
        color: scheme.errorContainer.withValues(alpha: 0.5),
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: scheme.error.withValues(alpha: 0.3)),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(Icons.error_outline_rounded, size: 15, color: scheme.error),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              error,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onErrorContainer,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// Shown between "request sent" and "first token" — the retrieval window.
class _ThinkingIndicator extends StatefulWidget {
  const _ThinkingIndicator();

  @override
  State<_ThinkingIndicator> createState() => _ThinkingIndicatorState();
}

class _ThinkingIndicatorState extends State<_ThinkingIndicator>
    with SingleTickerProviderStateMixin {
  late final AnimationController _c = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1100),
  )..repeat();

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (var i = 0; i < 3; i++)
            AnimatedBuilder(
              animation: _c,
              builder: (context, _) {
                final phase = (_c.value - i * 0.18) % 1.0;
                final t = phase < 0.5 ? phase * 2 : (1 - phase) * 2;
                return Container(
                  width: 6,
                  height: 6,
                  margin: const EdgeInsets.only(right: 5),
                  decoration: BoxDecoration(
                    shape: BoxShape.circle,
                    color: scheme.onSurfaceVariant.withValues(
                      alpha: 0.25 + 0.55 * t,
                    ),
                  ),
                );
              },
            ),
          const SizedBox(width: 4),
          Text(
            '正在检索记忆…',
            style: Theme.of(context).textTheme.labelSmall,
          ),
        ],
      ),
    );
  }
}

/// Blinking block caret trailing the streamed text.
class _Caret extends StatefulWidget {
  const _Caret();

  @override
  State<_Caret> createState() => _CaretState();
}

class _CaretState extends State<_Caret> with SingleTickerProviderStateMixin {
  late final AnimationController _c = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 640),
  )..repeat(reverse: true);

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(top: 2),
      child: FadeTransition(
        opacity: _c.drive(Tween(begin: 0.15, end: 1.0)),
        child: Container(
          width: 7,
          height: 15,
          decoration: BoxDecoration(
            color: Theme.of(context).colorScheme.primary,
            borderRadius: BorderRadius.circular(1.5),
          ),
        ),
      ),
    );
  }
}
