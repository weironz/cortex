import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../models/chat_message.dart';
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

  /// Pages older history in while keeping the reading position still.
  ///
  /// Prepending grows the list *above* the viewport, and a `ListView` measures
  /// its offset from the top — so without compensation the content the user was
  /// reading slides down by the height of the page that just arrived. Holding
  /// the distance from the **bottom** constant across the fetch is what makes
  /// the new messages appear above rather than shove the old ones away.
  Future<void> _loadEarlier(String sessionId) async {
    final before = _scroll.hasClients
        ? _scroll.position.maxScrollExtent - _scroll.position.pixels
        : null;
    await ref.read(chatControllerProvider.notifier).loadEarlier(sessionId);
    if (!mounted || before == null) return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!_scroll.hasClients) return;
      final target = _scroll.position.maxScrollExtent - before;
      _scroll.jumpTo(target.clamp(0.0, _scroll.position.maxScrollExtent));
    });
  }

  @override
  Widget build(BuildContext context) {
    // `ref.listen` reacts without rebuilding — exactly what auto-scroll needs.
    ref.listen(
      chatControllerProvider.select((s) => s.streaming?.text.length ?? 0),
      (_, _) => _scheduleScrollToBottom(),
    );
    // Keyed on the *newest* message rather than on the list length: paging
    // older history in also grows the list, and jumping to the bottom then
    // would throw the user out of exactly the part they scrolled up to read.
    ref.listen(
      chatControllerProvider.select(
        (s) => s.activeTranscript.isEmpty ? null : s.activeTranscript.last.id,
      ),
      (_, _) => _scheduleScrollToBottom(force: true),
    );
    ref.listen(
      chatControllerProvider.select((s) => s.activeSessionId),
      (_, _) => _scheduleScrollToBottom(force: true),
    );

    // One page's worth, because that is all that was fetched. The identity of
    // this list is stable across deltas — see [Transcript].
    final messages = ref.watch(
      chatControllerProvider.select((s) => s.activeTranscript),
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
    final loadingEarlier = ref.watch(
      chatControllerProvider.select(
        (s) => s.activeTranscriptState?.loadingEarlier ?? false,
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

    // 「还没开口」那一版由 `ChatPane` 整个接管了（招呼 + 居中的输入框 +
    // 起手式是一体的）。判据只留在它那一处 —— 这里再判一遍的话，两边差一个
    // 条件就是同一块招呼语出现两次，或者一次都不出现
    // One header slot above the first message: the button that fetches the
    // page before this one.
    final headers = hasEarlier ? 1 : 0;

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
              if (hasEarlier) {
                if (i == 0) {
                  return _LoadEarlier(
                    loading: loadingEarlier,
                    onTap: sessionId == null
                        ? null
                        : () => unawaited(_loadEarlier(sessionId)),
                  );
                }
                i -= 1;
              }
              if (i < messages.length) {
                final message = messages[i];
                return MessageBubble(
                  key: ValueKey(message.id),
                  message: message,
                  // 「有一轮在跑吗」与「这条回答对应哪句话」都由这里算好
                  // 再传下去，气泡自己不去读全局状态。
                  //
                  // 不这么做的话，一个纯展示的气泡在 build 期间就会把
                  // ChatController 拉起来 —— 单独渲染一条消息的那些测试
                  // 会连带触发一次拉列表，卡在一个没人处理的定时器上
                  busy: streaming,
                  retryTarget: message.role == MessageRole.assistant
                      ? _userMessageBefore(messages, i)
                      : null,
                );
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
/// 往前找「这条回答是回应哪句话的」。
///
/// 往前扫而不是按下标减一：工具行、被打断的轮次都可能插在中间，
/// 而「上一条用户消息」这个说法在任何排布下都成立。
String? _userMessageBefore(List<ChatMessage> messages, int at) {
  for (var i = at - 1; i >= 0; i--) {
    if (messages[i].role == MessageRole.user) return messages[i].id;
  }
  return null;
}

class _StreamingBubble extends ConsumerWidget {
  const _StreamingBubble();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = ref.watch(
      chatControllerProvider.select((s) => s.streaming?.text ?? ''),
    );
    final tools = ref.watch(
      chatControllerProvider.select((s) => s.streaming?.toolCalls ?? const []),
    );
    final queuedAhead = ref.watch(
      chatControllerProvider.select((s) => s.streaming?.queuedAhead),
    );

    return AssistantBlock(
      text: text,
      toolCalls: tools,
      streaming: true,
      queuedAhead: queuedAhead,
    );
  }
}

/// Fetches the page before this one.
///
/// This is a real request now, not a reveal: `GET /sessions/{id}` returns the
/// newest page by default and walks backwards through `?before=`, so the
/// messages above have genuinely not crossed the wire yet. It replaces both the
/// old client-side window *and* the banner that used to warn that a long
/// session's newest turns had been cut off — the server no longer drops them.
class _LoadEarlier extends StatelessWidget {
  const _LoadEarlier({required this.loading, this.onTap});

  final bool loading;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(kMessageGutter, 4, kMessageGutter, 12),
      child: Center(
        child: TextButton.icon(
          // Disabled while in flight rather than hidden: a button that vanishes
          // under the cursor makes the tap feel like it missed.
          onPressed: loading ? null : onTap,
          icon: loading
              ? const SizedBox(
                  width: 13,
                  height: 13,
                  child: CircularProgressIndicator(strokeWidth: 1.8),
                )
              : const Icon(Icons.expand_less_rounded, size: 17),
          label: Text(loading ? '正在加载…' : '加载更早的 $kEpisodePage 条'),
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
      shape: CircleBorder(side: BorderSide(color: scheme.outlineVariant)),
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

/// 空会话页上方那块招呼。
///
/// **只画自己，不摆位置** —— 摆在哪由 `ChatPane` 决定，因为居中那一版里
/// 它与输入框、起手式是同一根 `Column` 上的三段。此前它自带 `Center` +
/// 滚动容器，于是输入框只能钉在它下面的底边上。
class ConversationHero extends StatelessWidget {
  const ConversationHero({super.key});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return ConstrainedBox(
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
        ],
      ),
    );
  }
}

/// 几个起手式。放在输入框**下面** —— 它们是「不知道说什么时点一个」，
/// 而不是「先读完再开始打字」。
class ConversationPrompts extends ConsumerWidget {
  const ConversationPrompts({super.key});

  static const _prompts = [
    'Rust async trait 该怎么选？',
    'pgvector 的 HNSW 参数怎么调？',
    'Flutter 客户端应该怎么分层？',
    '帮我写这周的周报',
  ];

  @override
  Widget build(BuildContext context, WidgetRef ref) => ConstrainedBox(
    constraints: const BoxConstraints(maxWidth: 620),
    child: Wrap(
      spacing: 8,
      runSpacing: 8,
      alignment: WrapAlignment.center,
      children: [
        for (final prompt in _prompts)
          ActionChip(
            label: Text(prompt),
            onPressed: () =>
                ref.read(chatControllerProvider.notifier).send(prompt),
          ),
      ],
    ),
  );
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
