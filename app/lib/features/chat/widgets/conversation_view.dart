import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../state/chat_controller.dart';
import '../../../widgets/empty_state.dart';
import 'message_bubble.dart';

/// The scrolling transcript.
///
/// ## How streaming avoids a full-list rebuild
///
/// This widget watches only two things: the committed transcript's *list
/// identity* and a bool for "is a stream running". Neither changes while tokens
/// arrive — [ChatController] appends to `state.streaming.text` and leaves the
/// `transcripts` map untouched — so the `ListView` and every settled bubble are
/// never rebuilt mid-stream.
///
/// The live turn is rendered by [_StreamingBubble], which selects
/// `streaming.text` and is therefore the *only* widget that rebuilds per delta.
/// Because the text is appended (never replaced) and the bubble keeps its
/// element, `RenderParagraph` extends the existing layout rather than
/// re-flowing the message from scratch — no flicker, no scroll jump.
class ConversationView extends ConsumerStatefulWidget {
  const ConversationView({super.key});

  @override
  ConsumerState<ConversationView> createState() => _ConversationViewState();
}

class _ConversationViewState extends ConsumerState<ConversationView> {
  final ScrollController _scroll = ScrollController();

  /// Auto-follow is sticky: once the user scrolls up to read something, new
  /// tokens must not yank them back down. Re-armed when they return to bottom.
  bool _follow = true;

  @override
  void initState() {
    super.initState();
    _scroll.addListener(_onScroll);
  }

  @override
  void dispose() {
    _scroll.removeListener(_onScroll);
    _scroll.dispose();
    super.dispose();
  }

  void _onScroll() {
    if (!_scroll.hasClients) return;
    final atBottom =
        _scroll.position.pixels >= _scroll.position.maxScrollExtent - 48;
    if (atBottom != _follow) {
      setState(() => _follow = atBottom);
    }
  }

  void _scheduleScrollToBottom({bool force = false}) {
    if (!force && !_follow) return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!_scroll.hasClients) return;
      _scroll.jumpTo(_scroll.position.maxScrollExtent);
    });
  }

  @override
  Widget build(BuildContext context) {
    // `ref.listen` reacts without rebuilding — exactly what auto-scroll needs.
    ref.listen(
      chatControllerProvider.select((s) => s.streaming?.text.length ?? 0),
      (_, _) => _scheduleScrollToBottom(),
    );
    ref.listen(
      chatControllerProvider.select((s) => s.activeTranscript.length),
      (_, _) => _scheduleScrollToBottom(force: true),
    );
    ref.listen(
      chatControllerProvider.select((s) => s.activeSessionId),
      (_, _) => _scheduleScrollToBottom(force: true),
    );

    final messages = ref.watch(
      chatControllerProvider.select((s) => s.activeTranscript),
    );
    final streaming = ref.watch(
      chatControllerProvider.select((s) => s.isStreamingActive),
    );

    if (messages.isEmpty && !streaming) {
      return const _ConversationEmptyState();
    }

    return Stack(
      children: [
        Scrollbar(
          controller: _scroll,
          child: ListView.builder(
            controller: _scroll,
            padding: const EdgeInsets.symmetric(vertical: 16),
            // Keep the live bubble alive across scrolls so its animation
            // controllers are not torn down and rebuilt.
            addAutomaticKeepAlives: true,
            itemCount: messages.length + (streaming ? 1 : 0),
            itemBuilder: (context, index) {
              if (index < messages.length) {
                final message = messages[index];
                return MessageBubble(key: ValueKey(message.id), message: message);
              }
              return const _StreamingBubble();
            },
          ),
        ),
        if (!_follow)
          Positioned(
            right: 20,
            bottom: 16,
            child: _JumpToBottom(
              onTap: () {
                setState(() => _follow = true);
                _scheduleScrollToBottom(force: true);
              },
            ),
          ),
      ],
    );
  }
}

/// The single widget that rebuilds per SSE delta.
class _StreamingBubble extends ConsumerWidget {
  const _StreamingBubble();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = ref.watch(
      chatControllerProvider.select((s) => s.streaming?.text ?? ''),
    );
    final facts = ref.watch(
      chatControllerProvider.select((s) => s.streaming?.facts ?? const []),
    );
    final tools = ref.watch(
      chatControllerProvider.select((s) => s.streaming?.toolCalls ?? const []),
    );

    return AssistantBlock(
      text: text,
      facts: facts,
      toolCalls: tools,
      streaming: true,
    );
  }
}

class _JumpToBottom extends StatelessWidget {
  const _JumpToBottom({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      elevation: 2,
      color: scheme.surfaceContainerHigh,
      shape: CircleBorder(
        side: BorderSide(color: scheme.outlineVariant),
      ),
      clipBehavior: Clip.antiAlias,
      child: InkWell(
        onTap: onTap,
        child: SizedBox(
          width: 34,
          height: 34,
          child: Icon(
            Icons.arrow_downward_rounded,
            size: 17,
            color: scheme.onSurfaceVariant,
          ),
        ),
      ),
    );
  }
}

class _ConversationEmptyState extends ConsumerWidget {
  const _ConversationEmptyState();

  static const _prompts = [
    'Rust async trait 该怎么选？',
    'pgvector 的 HNSW 参数怎么调？',
    'Flutter 客户端应该怎么分层？',
    '帮我写这周的周报',
  ];

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(28),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 520),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Container(
                width: 52,
                height: 52,
                decoration: BoxDecoration(
                  gradient: LinearGradient(
                    colors: [scheme.primary, scheme.secondary],
                  ),
                  borderRadius: BorderRadius.circular(15),
                ),
                child: const Icon(
                  Icons.auto_awesome,
                  color: Colors.white,
                  size: 25,
                ),
              ),
              const SizedBox(height: 18),
              Text('记忆原生的 AI Agent', style: theme.textTheme.titleLarge),
              const SizedBox(height: 7),
              Text(
                '每一轮对话都会被原样归档，抽取出的事实可追溯、可审计、可回放。',
                textAlign: TextAlign.center,
                style: theme.textTheme.bodySmall,
              ),
              const SizedBox(height: 24),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                alignment: WrapAlignment.center,
                children: [
                  for (final prompt in _prompts)
                    ActionChip(
                      label: Text(prompt),
                      onPressed: () => ref
                          .read(chatControllerProvider.notifier)
                          .send(prompt),
                    ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Shown when nothing is selected at all.
class NoSessionState extends StatelessWidget {
  const NoSessionState({super.key, required this.onCreate});

  final VoidCallback onCreate;

  @override
  Widget build(BuildContext context) {
    return EmptyState(
      icon: Icons.forum_outlined,
      title: '还没有选中会话',
      description: '新建一个会话开始对话。',
      action: FilledButton.icon(
        onPressed: onCreate,
        icon: const Icon(Icons.add_rounded, size: 18),
        label: const Text('新建会话'),
      ),
    );
  }
}
