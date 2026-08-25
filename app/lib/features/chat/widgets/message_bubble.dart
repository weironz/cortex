import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/formatting.dart';
import '../../../core/theme.dart';
import '../../../core/link_launcher.dart';
import '../../../core/motion.dart';
import '../../../models/attachment.dart';
import '../../../models/chat_message.dart';
import '../../../models/tool_call.dart';
import '../../images/widgets/drawing_placeholder.dart';
import '../../../state/chat_controller.dart';
import '../../../state/model_controller.dart';
import '../../../state/composer_draft.dart';
import '../../../widgets/markdown/cortex_markdown.dart';
import 'attachment_views.dart';
import 'turn_drawer.dart';

/// Horizontal padding around the conversation column.
const kMessageGutter = 28.0;

/// Upper bound on line length. Long measure hurts readability badly in CJK,
/// and an ultrawide monitor would otherwise stretch a paragraph across 2000px.
const kMessageMaxWidth = 760.0;

/// 整条对话那一列有多宽。**与输入框同宽**。
///
/// [kMessageMaxWidth] 管的是单个气泡里那行字最长多少，而这一条管的是
/// **整列**——两者不是一回事，而此前只有前者。少了后者的表现是：宽窗口上
/// 助手的文字贴着左边、用户的气泡贴着右边，中间横跨一千多像素。
///
/// 取值 = 文本宽度 + 两侧 [kMessageGutter]，与 `message_composer` 里那个
/// 820 对齐 —— 消息列的边与输入框的边落在同一条竖线上。
const kConversationWidth = kMessageMaxWidth + kMessageGutter * 2;

/// A committed message.
///
/// Assistant text goes through [CortexMarkdown]; user text stays plain — echoing
/// the user's own input through a markdown parser would silently mangle any
/// `*` or `#` they typed literally.
class MessageBubble extends StatelessWidget {
  const MessageBubble({
    super.key,
    required this.message,
    this.busy = false,
    this.retryTarget,
  });

  final ChatMessage message;

  /// 这个会话此刻有一轮在跑吗。有的话重发与改写都按不动。
  ///
  /// 由 [ConversationView] 算好传进来，气泡自己不读全局状态 ——
  /// 一个纯展示的部件在 build 里把 controller 拉起来，会让「单独渲染
  /// 一条消息」的测试连带触发一次拉列表。
  final bool busy;

  /// 这条**回答**要重试时该重发哪一条用户消息。`null` = 找不到，不给按钮。
  final String? retryTarget;

  @override
  Widget build(BuildContext context) {
    return message.role == MessageRole.user
        ? _UserBubble(message: message, busy: busy)
        : AssistantBlock(
            text: message.text,
            toolCalls: message.toolCalls,
            attachments: message.attachments,
            createdAt: message.createdAt,
            episodeId: message.episodeId,
            error: message.error,
            busy: busy,
            retryTarget: retryTarget,
            models: message.models,
          );
  }
}

class _UserBubble extends ConsumerWidget {
  const _UserBubble({required this.message, required this.busy});

  final ChatMessage message;

  /// 有一轮在跑时两个动作都停用：`send` 那边本来就会直接返回，
  /// 而一个点了没反应的按钮比没有这个按钮更让人困惑。
  final bool busy;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
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
              // 放在气泡外面而不是里面：附件是**用户的内容**，被气泡的底色
              // 罩住一层之后会读成界面的一部分。
              //
              // （气泡此前是实心品牌色，那时这条理由更强烈；现在底色淡了，
              // 但结论不变 —— 一张图不该带着任何界面色。）
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
                    // ── 中性填充，不是实心品牌色 ──
                    //
                    // 这里原来是整块 `scheme.primary`。按新规范（见
                    // docs/design.md）色彩只表达**动作或含义**，而一条
                    // 用户消息是内容 —— Cherry Studio 的规矩里这一条写得
                    // 很直接：「不要把 primary 当成通用蓝或装饰色」。
                    //
                    // 两家参考产品都是这么做的：助手消息不带气泡（我们
                    // 已经如此），用户消息用一层很轻的中性底把它与助手的
                    // 输出分开。一屏对话里因此只剩下真正需要注意的地方
                    // 才有颜色。
                    color: scheme.surfaceContainerHigh,
                    // 气泡归辅助档 radiusLg(11) —— 五档体系里写明
                    // 「气泡 11」（见 CortexTokens）；Xl(13) 是卡片那一档
                    borderRadius: const BorderRadius.only(
                      topLeft: Radius.circular(CortexTokens.radiusLg),
                      topRight: Radius.circular(CortexTokens.radiusLg),
                      bottomLeft: Radius.circular(CortexTokens.radiusLg),
                      // 右下角收窄成一个「尾巴」。这一个不走圆角阶：
                      // 它不是装饰，是指向发送者的方向
                      bottomRight: Radius.circular(4),
                    ),
                  ),
                  child: SelectionArea(
                    child: Text(
                      message.text,
                      style: theme.textTheme.bodyLarge?.copyWith(
                        color: scheme.onSurface,
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
                    // 「改一改再发一次」而不是「编辑」。
                    //
                    // 历史是 append-only，这条消息改不掉 —— 它会以新的一轮
                    // 追加在末尾，旧的那一轮留在原地。叫「编辑」的话，
                    // 用户会以为上面那句被换掉了，直到他换一台设备回放才发现
                    // 它还在，而那时他可能已经在里面写过不该留下的东西
                    _BubbleAction(
                      icon: Icons.edit_note_rounded,
                      tooltip: busy ? '这一轮跑完才能改' : '改一改再发一次（原来那条留在历史里）',
                      onPressed: busy
                          ? null
                          : () => ref
                                .read(composerDraftProvider.notifier)
                                .offer(message.text),
                    ),
                    const SizedBox(width: 4),
                  ],
                  _BubbleAction(
                    icon: Icons.refresh_rounded,
                    tooltip: busy ? '这一轮跑完才能重发' : '原样再发一次',
                    onPressed: busy
                        ? null
                        : () => ref
                              .read(chatControllerProvider.notifier)
                              .resend(message.id),
                  ),
                  const SizedBox(width: 4),
                  _ForkHereButton(episodeId: message.episodeId, busy: busy),
                  const SizedBox(width: 4),
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
                  child: TurnDrawer(toolCalls: message.toolCalls),
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
    this.queuedAhead,
    this.busy = false,
    this.retryTarget,
    this.models = const [],
  });

  final String text;
  final List<ToolCall> toolCalls;
  final List<Attachment> attachments;
  final DateTime? createdAt;
  final String? episodeId;
  final String? error;
  final bool streaming;

  /// 这一轮还排在队里，前面有几轮。`null` = 没排队（绝大多数情况）。
  final int? queuedAhead;

  /// 这条回复**先后**是谁写的。空 = 不知道，什么都不画。
  final List<String> models;

  /// 这个会话此刻有一轮在跑吗。
  final bool busy;

  /// 出错时「重试」该重发哪一条用户消息。`null` = 没有可重试的对象
  /// （直播中的那一轮、或者前面压根没有用户消息）。
  final String? retryTarget;

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
                    // 纯色，不渐变（渐变只留发送键一处 —— 头像是身份不是动作）
                    decoration: BoxDecoration(
                      color: scheme.primary,
                      borderRadius: BorderRadius.circular(
                        CortexTokens.radiusSm,
                      ),
                    ),
                    // onPrimary，不写死白色：深色主题下 primary 是**浅**靛，
                    // 白色叠上去只有 3.2:1，onPrimary（深靛）才读得清
                    child: Icon(
                      Icons.auto_awesome,
                      size: 12,
                      color: scheme.onPrimary,
                    ),
                  ),
                  const SizedBox(width: 8),
                  Text('Cortex', style: theme.textTheme.titleSmall),
                  // 谁写的。**流式过程中不画** —— 那时还不知道，
                  // 而先画一个再换掉会闪
                  if (models.isNotEmpty && !streaming) ...[
                    const SizedBox(width: 8),
                    Flexible(child: _ModelTrail(models: models)),
                  ],
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
                    // 正在画图 —— 占住那张图**将来的位置**。
                    //
                    // 判据是「有一次 generate_image 还没回结果」，而不是
                    // 「这一轮在跑」：一轮里可能先读文件再画图，前半段画一块
                    // 空图位是在承诺一件还没发生的事。
                    //
                    // 图回来之后这一行会变成 `AttachmentStrip` 里的缩略图，
                    // 而占位随 pending 消失 —— 两者不会同时出现
                    for (final _ in toolCalls.where(
                      (t) => t.name == 'generate_image' && t.pending,
                    )) ...[
                      const DrawingPlaceholder(),
                      const SizedBox(height: 8),
                    ],
                    if (text.isNotEmpty)
                      SelectionArea(
                        child: CortexMarkdown(
                          text,
                          onLinkTap: (url) => openExternalLink(context, url),
                        ),
                      ),
                    if (streaming) ...[
                      if (text.isEmpty)
                        // 排队中就把「前面还有几轮」说出来，而不是一个和
                        // 「等首 token」长得一样的省略号 —— 那两件事在屏幕上
                        // 分不开的话，用户会以为模型卡住了
                        if (queuedAhead case final ahead?)
                          _QueuedNote(ahead: ahead)
                        else
                          const _ThinkingIndicator()
                      else
                        const _Caret(),
                    ],
                    if (error != null)
                      _ErrorNote(
                        error: error!,
                        busy: busy,
                        retryTarget: retryTarget,
                      ),
                    TurnDrawer(toolCalls: toolCalls, streaming: streaming),
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
                            _ForkHereButton(episodeId: episodeId, busy: busy),
                            if (episodeId != null)
                              Padding(
                                padding: const EdgeInsets.only(left: 5),
                                child: Text(
                                  'episode ${shortId(episodeId)}',
                                  style: theme.textTheme.labelSmall?.copyWith(
                                    fontFamily: 'JetBrains Mono',
                                    fontFamilyFallback:
                                        CortexTheme.monoFallback,
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

class _ErrorNote extends ConsumerWidget {
  const _ErrorNote({required this.error, this.busy = false, this.retryTarget});
  final String error;
  final bool busy;

  /// 找得到对应的那句用户消息才给「重试」。找不到（历史翻页翻掉了、
  /// 或者这一块前面压根没有用户消息）时不给一个点下去什么都不发生的按钮。
  final String? retryTarget;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final target = retryTarget;
    return Container(
      margin: const EdgeInsets.only(top: 8),
      padding: const EdgeInsets.symmetric(horizontal: 11, vertical: 8),
      decoration: BoxDecoration(
        color: scheme.errorContainer.withValues(alpha: 0.5),
        borderRadius: BorderRadius.circular(CortexTokens.radiusMd),
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
          if (target != null) ...[
            const SizedBox(width: 8),
            TextButton.icon(
              onPressed: busy
                  ? null
                  : () => ref
                        .read(chatControllerProvider.notifier)
                        .resend(target),
              icon: const Icon(Icons.refresh_rounded, size: 15),
              label: const Text('重试'),
              style: TextButton.styleFrom(
                foregroundColor: scheme.error,
                padding: const EdgeInsets.symmetric(horizontal: 8),
                minimumSize: Size.zero,
                tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                visualDensity: VisualDensity.compact,
              ),
            ),
          ],
        ],
      ),
    );
  }
}

/// 「发出去了，还没有第一个 token」——**只表达这一件事**。
///
/// # 它以前写着「正在检索记忆…」，那是编的
///
/// 触发条件是 `streaming && text.isEmpty`，也就是**任何**等首 token 的时刻：
/// 模型慢、先要调工具、这一轮压根没检索、甚至这个部署根本没接记忆服务
/// （`memory_reachable: false`）——四种情况下它都言之凿凿地说在检索记忆。
///
/// 更根本的问题是**客户端无从知道**：流上只有 delta / tool / confirm /
/// done / error 五种事件，没有任何一条讲检索。那句话不是过期，是从来就没有
/// 依据 —— 原注释里那句「the retrieval window」把「等首 token 的窗口」
/// 当成了「检索的窗口」。
///
/// 所以现在只留动效：**「有事在发生」是真的，「在做什么」我们不知道**。
/// 要说得更具体，得先让服务端发一条真的阶段事件；在那之前，
/// 一个诚实的省略号胜过一句好看的假话。
/// 「这句话排在队里，前面还有几轮」。
///
/// # 为什么它不是 [_ThinkingIndicator] 加一行字
///
/// 那个指示器的全部意思是「发出去了，还没有第一个 token」——它刻意不说在做
/// 什么，因为客户端无从知道（见它自己的文档）。而这里是**服务端真的说了**
/// 的一件事：`queued` 事件带着 `ahead`。两者的区别正是「有依据」与「没依据」，
/// 所以分成两个部件，而不是给一个部件加一个可选的说明文字。
class _QueuedNote extends StatelessWidget {
  const _QueuedNote({required this.ahead});

  final int ahead;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Text(
        // 说清楚是**这个会话**在忙，而不是服务不可用 —— 后者会让人去刷新页面
        '这个会话前面还有 $ahead 轮在跑，你这句排在后面，跑完就接着答。',
        style: theme.textTheme.bodySmall?.copyWith(
          color: theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

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
  );

  // 「减少动效」开着时停在终态：三个点的透明度公式在 t=1 处最实，
  // 于是它变成三个静止的点，而不是消失
  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    syncLoop(_c, context);
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Semantics(
      // 读屏要有个说法，而这是唯一说得准的那句：请求发出去了，还没回来。
      // 视觉上不显示它 —— 一行「等待回复」对看得见动效的人是冗余的
      label: '等待回复',
      child: Padding(
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
          ],
        ),
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
  );

  // 停在 1 = 完全不透明。停在 0 的话光标会**消失**，而它此刻正是
  // 「还在输出」的唯一视觉证据
  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    syncLoop(_c, context, reverse: true);
  }

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
            // **不走圆角那五阶**：它在画一个**字形**（文本光标），
            // 不是一个容纳内容的面。宽 7 的条子，半宽才 3.5 ——
            // 取最小的 radiusSm（6）会直接变成一颗药丸
            borderRadius: BorderRadius.circular(1.5),
          ),
        ),
      ),
    );
  }
}

/// 「从这里分叉」—— 到这条消息为止的历史进入一条新会话，原会话不动。
///
/// # 为什么按钮自己是 ConsumerWidget
///
/// 它要拿两样只有 provider 里才有的东西：当前会话 id 与控制器。让
/// [AssistantBlock] 整个变成 Consumer 的代价是流式路径上每次状态变化都
/// 多一层重建判断，而这颗按钮只在一轮结束后才出现。
///
/// # 为什么锚是 episodeId 而不是客户端消息 id
///
/// 服务端认的是**落库的** episode id。刚发出去、还没收到回执的消息只有
/// 客户端本地 id —— 那时按钮停用，tooltip 说清等一会儿，而不是把一个
/// 服务端不认识的 id 发过去换一条 400。
class _ForkHereButton extends ConsumerWidget {
  const _ForkHereButton({required this.episodeId, required this.busy});

  final String? episodeId;

  /// 这个会话有一轮在跑。跑着的时候历史还在长，「到这里」指不稳。
  final bool busy;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final enabled = !busy && episodeId != null;
    return _BubbleAction(
      icon: Icons.call_split_rounded,
      tooltip: busy
          ? '这一轮跑完才能分叉'
          : episodeId == null
          ? '这条还没写进历史，稍等片刻再分叉'
          : '从这里分叉：到这条为止的历史进入新会话，这条会话不动',
      onPressed: !enabled
          ? null
          : () async {
              final messenger = ScaffoldMessenger.maybeOf(context);
              final sessionId = ref
                  .read(chatControllerProvider)
                  .activeSessionId;
              if (sessionId == null) return;
              final said = await ref
                  .read(chatControllerProvider.notifier)
                  .forkSession(sessionId, upToEpisodeId: episodeId);
              if (said != null) {
                messenger?.showSnackBar(SnackBar(content: Text(said)));
              }
            },
    );
  }
}

/// 气泡下面那一行里的一个小动作按钮。
///
/// 与 [_CopyButton] 同一个尺寸与手感，但不带「已复制」那个瞬时状态 ——
/// 重发与改写的反馈是屏幕上真的多了一条消息 / 输入框里出现了文字，
/// 不需要按钮自己再说一遍。
class _BubbleAction extends StatelessWidget {
  const _BubbleAction({
    required this.icon,
    required this.tooltip,
    required this.onPressed,
  });

  final IconData icon;
  final String tooltip;

  /// `null` = 停用。停用而不是整个藏起来：按钮忽隐忽现会让这一行的宽度
  /// 在每一轮开始和结束时跳一下，而 tooltip 还能说清为什么现在点不了
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return IconButton(
      onPressed: onPressed,
      icon: Icon(icon, size: 15),
      tooltip: tooltip,
      color: scheme.onSurfaceVariant,
      padding: EdgeInsets.zero,
      constraints: const BoxConstraints.tightFor(width: 26, height: 22),
      visualDensity: VisualDensity.compact,
      splashRadius: 14,
    );
  }
}

/// 「这条回复是谁写的」。
///
/// # 为什么显示的是目录里的名字，不是原始 id
///
/// 服务端记下来的是模型 id（`gemini-3-pro-image-preview`），而人认得的是
/// 显示名（`Gemini 3 Pro Image`）。目录客户端本来就加载了（撰写框那个
/// 选择器用它），顺手查一下不多花一次请求。
///
/// **查不到就显示原始 id**，不显示「未知模型」—— 目录跟着 models.dev 走，
/// 新发的型号会有一段空窗期，那时原始 id 仍然是有用信息。
///
/// # 为什么可能有好几个
///
/// 一次回复要跑好几轮模型调用（有几次工具调用就有几轮），而「自动」档是
/// **按请求**挑模型的 —— 于是可能先用便宜的跑工具、再用贵的写答案。
/// 用 `→` 连起来，按发生顺序。
class _ModelTrail extends ConsumerWidget {
  const _ModelTrail({required this.models});

  final List<String> models;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    // ⚠️ 目录**没拉到也要照画**（用原始 id）。等它才画的话，
    // 一个连不上 `/llm/models` 的部署里这一行永远不出现，
    // 而那与「这条消息没记模型」看起来一模一样
    final catalog = ref.watch(modelCatalogProvider).value;
    final label = models
        .map((id) => catalog?.byId(id)?.displayName ?? id)
        .join(' → ');

    return Tooltip(
      // 悬停给原始 id：显示名可能与配置里要填的那个不一样，
      // 而要照着抄的人抄的是 id
      message: models.join(' → '),
      child: Text(
        label,
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
        style: theme.textTheme.labelSmall?.copyWith(
          color: theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}
