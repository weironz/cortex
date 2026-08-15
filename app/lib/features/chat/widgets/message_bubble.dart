import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../../core/formatting.dart';
import '../../../core/link_launcher.dart';
import '../../../models/attachment.dart';
import '../../../models/chat_message.dart';
import '../../../models/tool_call.dart';
import '../../../widgets/markdown/cortex_markdown.dart';
import 'attachment_views.dart';
import 'turn_drawer.dart';

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
            toolCalls: message.toolCalls,
            attachments: message.attachments,
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
              // Above the bubble rather than inside it: an image tinted by the
              // primary-coloured bubble reads as part of the UI instead of as
              // content the user attached.
              AttachmentStrip(attachments: message.attachments, alignEnd: true),
              // A message can legitimately be attachments only — "看这张图" with
              // a screenshot and nothing else. An empty bubble next to the
              // image would read as a failed send.
              if (message.text.isNotEmpty)
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
              // 时间与复制并排，而不是给复制单开一行：这一行本来就在那儿，
              // 塞进去不占额外高度。自己说过的话同样需要能整段拿走 ——
              // 拿去重发、拿去贴进别处，都比在气泡里手动划选可靠
              Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  if (message.text.isNotEmpty) ...[
                    _CopyButton(text: message.text, tooltip: '复制这条'),
                    const SizedBox(width: 4),
                  ],
                  Text(
                    formatRelative(message.createdAt),
                    style: theme.textTheme.labelSmall,
                  ),
                ],
              ),
              // Normally empty: `ChatController` carries a turn's attribution
              // onto the answer, where the drawer belongs. It survives here for
              // the one case that has no answer — a turn the model failed still
              // injected memory and may have run tools, and that is precisely
              // the turn somebody comes back to look at.
              if (message.toolCalls.isNotEmpty)
                Align(
                  alignment: Alignment.centerLeft,
                  child: TurnDrawer(
                    toolCalls: message.toolCalls,
                  ),
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
    required this.toolCalls,
    this.attachments = const [],
    this.createdAt,
    this.episodeId,
    this.error,
    this.streaming = false,
  });

  final String text;
  final List<ToolCall> toolCalls;
  final List<Attachment> attachments;
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
                ],
              ),
              const SizedBox(height: 6),
              Padding(
                padding: const EdgeInsets.only(left: 29),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    AttachmentStrip(attachments: attachments),
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
                    TurnDrawer(
                      toolCalls: toolCalls,
                      streaming: streaming,
                    ),
                    // 这一行动作 + 元信息，贴在回答**底部左侧**。
                    //
                    // 复制原来挂在头部那一行的最右端：离正文最远的那个角，
                    // 而人读完一段回答时视线落在左下。Claude Code / ChatGPT
                    // 都把它放这儿，不是审美偏好 —— 是「读完之后手往哪儿去」。
                    //
                    // 流式期间整行不出现：复制一段还在长的文本，拿到的是
                    // 半句话，而按钮看不出这一点。
                    if (!streaming && (text.isNotEmpty || episodeId != null))
                      Padding(
                        padding: const EdgeInsets.only(top: 2, left: 2),
                        child: Row(
                          children: [
                            if (text.isNotEmpty) _CopyButton(text: text),
                            if (episodeId != null)
                              Padding(
                                padding: const EdgeInsets.only(left: 5),
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
            ],
          ),
        ),
      ),
    );
  }
}

class _CopyButton extends StatefulWidget {
  const _CopyButton({required this.text, this.tooltip = '复制回答'});

  final String text;

  /// 悬停提示。assistant 那边是「复制回答」，用户自己那条是「复制这条」
  final String tooltip;

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
      tooltip: _done ? '已复制' : widget.tooltip,
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
          Text('正在检索记忆…', style: Theme.of(context).textTheme.labelSmall),
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
