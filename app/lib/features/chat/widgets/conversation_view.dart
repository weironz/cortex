import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../state/chat_controller.dart';
import '../../../state/chat_state.dart';
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

    // `visible` rather than the full list: history replay can drop hundreds of
    // turns in at once, and each bubble costs a markdown parse. The identity of
    // this list is stable across deltas — see [Transcript].
    final messages = ref.watch(
      chatControllerProvider.select((s) => s.activeVisibleMessages),
    );
    final streaming = ref.watch(
      chatControllerProvider.select((s) => s.isStreamingActive),
    );
    final sessionId = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    final loading = ref.watch(
      chatControllerProvider.select(
        (s) => s.activeTranscriptState?.loading ?? false,
      ),
    );
    final error = ref.watch(
      chatControllerProvider.select((s) => s.activeTranscriptState?.error),
    );
    final hasEarlier = ref.watch(
      chatControllerProvider.select(
        (s) => s.activeTranscriptState?.hasEarlier ?? false,
      ),
    );
    final serverTruncated = ref.watch(
      chatControllerProvider.select(
        (s) => s.activeTranscriptState?.serverTruncated ?? false,
      ),
    );

    if (error != null && messages.isEmpty) {
      return EmptyState(
        icon: Icons.history_toggle_off_rounded,
        title: '拉不到这个会话的消息',
        description: error,
        tone: EmptyStateTone.error,
        action: OutlinedButton.icon(
          onPressed: sessionId == null
              ? null
              : () => ref
                    .read(chatControllerProvider.notifier)
                    .loadTranscript(sessionId),
          icon: const Icon(Icons.refresh_rounded, size: 16),
          label: const Text('重试'),
        ),
      );
    }

    if (loading && messages.isEmpty) {
      return const Center(
        child: SizedBox(
          width: 20,
          height: 20,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      );
    }

    if (messages.isEmpty && !streaming) {
      return const _ConversationEmptyState();
    }

    // Header slots sit above the first message: "load earlier" when the client
    // is holding back history, and the truncation warning when the *server* is.
    final headers = (hasEarlier ? 1 : 0) + (serverTruncated ? 1 : 0);

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
            itemCount: headers + messages.length + (streaming ? 1 : 0),
            itemBuilder: (context, index) {
              var i = index;
              if (serverTruncated) {
                if (i == 0) return const _ServerTruncatedNote();
                i -= 1;
              }
              if (hasEarlier) {
                if (i == 0) {
                  return _LoadEarlier(
                    onTap: sessionId == null
                        ? null
                        : () => ref
                              .read(chatControllerProvider.notifier)
                              .revealEarlier(sessionId),
                  );
                }
                i -= 1;
              }
              if (i < messages.length) {
                final message = messages[i];
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

/// Grows the render window. The messages are already in memory — this is a
/// rendering budget, not a fetch.
class _LoadEarlier extends StatelessWidget {
  const _LoadEarlier({this.onTap});

  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(kMessageGutter, 4, kMessageGutter, 12),
      child: Center(
        child: TextButton.icon(
          onPressed: onTap,
          icon: const Icon(Icons.expand_less_rounded, size: 17),
          label: Text('加载更早的 $kTranscriptWindow 条'),
        ),
      ),
    );
  }
}

/// The server, not the client, is holding history back.
///
/// `GET /sessions/{id}` is `LIMIT 500` over `ORDER BY occurred_at ASC` with no
/// cursor, so what is missing from a long session is its **most recent** turns.
/// Silently showing a conversation whose ending has been cut off is the kind of
/// wrong that never gets reported as a bug, because it looks like data.
class _ServerTruncatedNote extends StatelessWidget {
  const _ServerTruncatedNote();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Padding(
      padding: const EdgeInsets.fromLTRB(kMessageGutter, 4, kMessageGutter, 12),
      child: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: kMessageMaxWidth),
          child: Container(
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 9),
            decoration: BoxDecoration(
              color: scheme.surfaceContainerHigh,
              borderRadius: BorderRadius.circular(9),
              border: Border.all(color: scheme.outlineVariant),
            ),
            child: Row(
              children: [
                Icon(
                  Icons.history_rounded,
                  size: 15,
                  color: scheme.onSurfaceVariant,
                ),
                const SizedBox(width: 9),
                Expanded(
                  child: Text(
                    '这个会话的消息比服务端单次能返回的多。'
                    'cortexd 目前一次最多回 500 条且没有游标，'
                    '所以下面看到的是较早的部分，最新几轮可能不在其中。',
                    style: theme.textTheme.labelSmall,
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
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
