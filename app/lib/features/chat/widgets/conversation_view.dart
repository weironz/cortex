import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../models/chat_message.dart';
import '../../../state/chat_controller.dart';
import '../../../state/chat_state.dart';
import '../../../widgets/empty_state.dart';
import 'message_bubble.dart';
import '../../../core/theme.dart';

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

  /// 最多追几帧。见 [_pinToBottom] —— **必须有上限**：没有上限的自愈动作
  /// 在判据不收敛时就是个永动机（这个仓库为此吃过亏）。12 帧 ≈ 200ms，
  /// 足够一屏一屏排完一页历史，而排不完时停下来也只是「没到最底」，
  /// 不是卡死。
  static const int _maxPinFrames = 12;

  void _scheduleScrollToBottom({bool force = false}) {
    if (!force && !_follow) return;
    _pinToBottom(0, -1);
  }

  /// 贴到最底 —— **一帧是不够的，要追到 `maxScrollExtent` 不再长**。
  ///
  /// # 为什么单跳一次会停在半路
  ///
  /// 这是个懒构建、变高条目的 `ListView.builder`：没排过版的条目它没法知道
  /// 多高，于是 `maxScrollExtent` 是**按已排版部分外推的估算值**。列表刚换成
  /// 另一个会话时只排了一屏，那个估算远小于真实高度 —— 跳过去、更多条目
  /// 跟着排版、extent 变大，而位置留在原地，看起来就是**停在靠上的地方**。
  ///
  /// 2026-08-30 实报：「切换对话后再回去，滚动条不在最下面」。切**回**旧会话
  /// 最容易中招：transcript 已经缓存着，`activeTranscript.last.id` 不变，
  /// 于是那条按最新消息触发的 force 监听**不响**，只剩会话切换这一条跳一次。
  /// 首次打开时消息是异步到的，`last.id` 变化会再触发几次，反而蒙对了 ——
  /// 「有时对有时不对」的来源就在这儿。
  ///
  /// # 为什么不改成 `reverse: true`
  ///
  /// 那是结构上更对的解法（底部即 offset 0，不需要任何估算），但它会把条目
  /// 顺序反过来，而 `_loadEarlier` 的位置补偿、页头那个「加载更早」的槽位
  /// 都是按正序写的。为一个滚动位置去翻转整块渲染，换来的风险比买到的多。
  /// [lastExtent] 是**上一帧**看到的 `maxScrollExtent`；`-1` = 还没看过。
  ///
  /// ⚠️ **判「长没长」必须跨帧，不能在 `jumpTo` 之后当场判。**
  /// 第一版就是当场判的：那一刻新的排版还没跑，`pixels` 恰好等于当时的
  /// extent，于是看起来永远「已经到底」，追一次就停 —— 与只跳一帧没有区别。
  /// 实测里第一次打开就差 7440 像素，是这条测试把它抓出来的。
  void _pinToBottom(int attempt, double lastExtent) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || !_scroll.hasClients) return;
      final extent = _scroll.position.maxScrollExtent;
      // 与上一帧比：不再长了、而且人已经在底上 —— 到位了，收工
      final settled =
          extent == lastExtent && _scroll.position.pixels >= extent - 1;
      if (settled) return;
      if (_scroll.position.pixels < extent) {
        _scroll.jumpTo(extent);
      }
      if (attempt + 1 < _maxPinFrames) {
        _pinToBottom(attempt + 1, extent);
      }
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
            // ── 每一条都收进同一列 ──
            //
            // 收在**每一项**上而不是把整个 ListView 包起来：后者会让
            // 窗口两侧那几百像素不再响应滚轮，而一个「滚不动的空白区」
            // 在宽屏上占了半个界面。
            itemBuilder: (context, index) => _column(
              _item(
                context,
                index,
                messages,
                streaming,
                hasEarlier,
                loadingEarlier,
                sessionId,
              ),
            ),
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

  /// 把一条消息放进居中的内容列。
  ///
  /// # 为什么必须有这一层
  ///
  /// `kMessageMaxWidth` 此前只约束**单个气泡**的宽度，整列没有约束 ——
  /// 于是在一个 1400px 的窗口上，助手的文字贴着左边、用户的气泡贴着右边，
  /// 中间横跨一千多像素。眼睛要在两端来回跑，而这正是两家参考产品
  /// （Cherry Studio / LobeHub）都不这么做的原因：对话是**一列**。
  ///
  /// 宽度与输入框对齐（同为 [kConversationWidth]）：消息列的边与输入框的边
  /// 落在同一条竖线上，是「这些东西属于一起」最省力的表达。
  static Widget _column(Widget child) => Center(
    child: ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: kConversationWidth),
      child: child,
    ),
  );

  Widget _item(
    BuildContext context,
    int index,
    List<ChatMessage> messages,
    bool streaming,
    bool hasEarlier,
    bool loadingEarlier,
    String? sessionId,
  ) {
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
    final blocks = ref.watch(
      chatControllerProvider.select((s) => s.streaming?.blocks ?? const []),
    );
    final queuedAhead = ref.watch(
      chatControllerProvider.select((s) => s.streaming?.queuedAhead),
    );

    return AssistantBlock(
      text: text,
      toolCalls: tools,
      blocks: blocks,
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
            // 纯色，不渐变（设计稿：渐变**只留发送键一处**）。
            // 渐变一多，「主要动作」就不再突出 —— 头像是身份不是动作
            decoration: BoxDecoration(
              color: scheme.primary,
              borderRadius: BorderRadius.circular(CortexTokens.radiusXl),
            ),
            // onPrimary，不写死白色：深色主题下 primary 是**浅**靛，
            // 白色叠上去只有 3.2:1，onPrimary（深靛）才读得清
            child: Icon(Icons.auto_awesome, color: scheme.onPrimary, size: 25),
          ),
          const SizedBox(height: 18),
          // 只写当下真的成立的能力（CLAUDE.md 约束 2）：长期记忆
          // 2026-08-17 已拆去 Cormex，「抽取的事实可追溯、可回放」在
          // 这一侧不再成立 —— 留着就是每次打开都在骗人
          Text('通用 AI Agent', style: theme.textTheme.titleLarge),
          const SizedBox(height: 7),
          Text(
            '对话跨设备实时同步；可以调用工具、读写工作区里的文件。',
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
