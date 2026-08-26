import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../../widgets/panel_header.dart';
import '../workspace/right_rail.dart'
    show kRailToggleFilled, kRailToggleOutlined;
import '../settings/pages/model_picker.dart';
import '../shell/widgets/sync_indicator.dart';
import 'session_changes_sheet.dart';
import 'widgets/awaiting_confirm_pill.dart';
import 'widgets/confirm_panel.dart';
import 'widgets/conversation_view.dart';
import 'widgets/message_composer.dart';
import '../../core/theme.dart';

/// Centre pane: header, transcript, composer.
///
/// # 为什么它是有状态的：焦点
///
/// 对话区里**没有任何可聚焦的东西**（一堆文字与气泡），所以点它一下
/// 不会把键盘焦点从别处摘走。而「别处」包括右栏那个真终端 —— 于是
/// 用户在终端里敲完命令、回来点一下对话、接着打字，字**全进了终端**。
///
/// 接住那一下的只能是这一层：只有它知道「对话区」的范围，也只有它能
/// 把焦点交给输入框。所以输入框的焦点节点由它持有（见 [_composerFocus]）。
class ChatPane extends ConsumerStatefulWidget {
  const ChatPane({
    super.key,
    this.onToggleSessions,
    this.onSelectPanel,
    this.onOpenPanel,
    this.sessionsVisible = false,
    this.activePanel,
  });

  /// 收起 / 展开左栏。窄到放不下内联时，由 `AppShell` 换成「开抽屉」。
  final VoidCallback? onToggleSessions;

  /// 点右侧某个面板的图标。窄屏时 `AppShell` 会顺手把抽屉打开。
  final void Function(RightPanel panel)? onSelectPanel;

  /// **确保**右栏开着并显示某面板 —— 与 [onSelectPanel] 的差别是语义：
  /// 那个是开关（同面板再点一次 = 收起），这个是「带我去」。
  /// 「本会话改动」要的是后者 —— 右栏已经开着时点它，收起整栏是事故。
  final void Function(RightPanel panel)? onOpenPanel;

  /// 左栏此刻是不是内联可见 —— 决定图标朝哪边、tooltip 说「显示」还是「隐藏」。
  final bool sessionsVisible;

  /// 右侧此刻内联显示着谁（`null` = 收起，或者当前宽度只能开抽屉）。
  /// 决定两个图标里哪一个是实心的。
  final RightPanel? activePanel;

  @override
  ConsumerState<ChatPane> createState() => _ChatPaneState();
}

class _ChatPaneState extends ConsumerState<ChatPane> {
  /// 输入框的焦点。**由这一层持有**，理由见 [ChatPane] 的文档。
  final FocusNode _composerFocus = FocusNode(debugLabel: 'composer');

  /// 「焦点此刻在不在这一栏里」的探针。
  ///
  /// 不可聚焦、不参与遍历 —— 它唯一的作用是让 [_reclaimFocus] 问得出
  /// `hasFocus`（自己或任何后代拿着主焦点）。
  final FocusNode _paneProbe = FocusNode(
    debugLabel: 'chat-pane',
    skipTraversal: true,
    canRequestFocus: false,
  );

  @override
  void dispose() {
    _composerFocus.dispose();
    _paneProbe.dispose();
    super.dispose();
  }

  /// 在这一栏里按下了指针 —— 把键盘要回来。
  ///
  /// # ⚠️ 焦点已经在这一栏里时**什么都不做**
  ///
  /// 少了这个判断，每一次在气泡上按下指针都会把焦点抢给输入框，而消息
  /// 正文是可选中的（`SelectionArea`）—— 选中区在失去焦点时会被清掉，
  /// 于是「按住拖选一段话」变成选不动。
  ///
  /// 判据交给探针节点：它的 `hasFocus` 在任何后代（输入框、气泡上的按钮、
  /// 选中区）拿着主焦点时为真。
  ///
  /// # 为什么是 `Listener` 而不是 `GestureDetector`
  ///
  /// 手势要在竞技场里排队，而消息气泡里那些选中区与按钮各有自己的
  /// 识别器，且它们在里层 —— 一个套在外面的 tap 识别器在**大部分**
  /// 点击上都会输掉，表现是「点空白处管用、点在字上不管用」。
  /// `Listener` 不进竞技场，指针一按下就到。
  void _reclaimFocus(PointerDownEvent _) {
    if (_paneProbe.hasFocus) return;
    _composerFocus.requestFocus();
  }

  @override
  Widget build(BuildContext context) {
    final controller = ref.read(chatControllerProvider.notifier);
    final sessionId = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    final title = ref.watch(
      chatControllerProvider.select((s) => s.activeSession?.title),
    );
    final streaming = ref.watch(
      chatControllerProvider.select((s) => s.isStreamingActive),
    );
    // 「还没开口」——**判据只此一处**。放在 `ConversationView` 里各判一遍的话，
    // 两边差一个条件就是「招呼语和输入框同时出现在两个地方」，
    // 而那种错位只有真跑起来才看得见
    final empty = ref.watch(
      chatControllerProvider.select(
        (s) =>
            s.activeTranscript.isEmpty &&
            !s.isStreamingActive &&
            !(s.activeTranscriptState?.loading ?? false),
      ),
    );

    /// 横幅 + 确认面板 + 输入框，**一整摞**。
    ///
    /// 两种形态（居中 / 钉底）共用它：把横幅只接在其中一条路上，另一条上
    /// 的发送失败就没人报 —— 而「开不出这次对话的工作目录」正是从那条
    /// 空会话路上冒出来的。
    Widget composer({required bool centred}) => Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const _OfflineBanner(),
        const _SendErrorBanner(),
        // 紧挨着输入框：一轮正在跑时用户的眼睛就在这儿，而且离发送键近到
        // 划不过去。为什么不是模态，见 `ConfirmPanel`
        const ConfirmPanel(),
        MessageComposer(
          // ⚠️ **不能是 `hasSession`。** 惰性建会话之后白纸上没有 id，
          // 那样写会把输入框整个禁用 —— 而白纸恰恰是要在这里开口的地方，
          // 用户会看到一个点不动的输入框，以为应用坏了。
          //
          // 从前这么写是对的：那时「没有会话」意味着「你还没选中任何东西」，
          // 而现在它意味着「新对话」。同一段代码，语义被脚下换掉了
          enabled: true,
          sessionId: sessionId,
          // 附件入口要的：白纸上拖/选/粘一张图，会话在那一刻兑现
          ensureSession: controller.materializeSession,
          streaming: streaming,
          centred: centred,
          // 焦点节点由这一层给 —— 「点对话区就能接着打字」靠它
          focusNode: _composerFocus,
          onSend: (text, attachments) =>
              controller.send(text, attachments: attachments),
          onStop: controller.stopGeneration,
        ),
      ],
    );

    // 在这一栏里按下指针就把键盘要回来（除非焦点已经在栏内）。
    // 探针套在整栏外面，所以「栏内」包括输入框、气泡上的按钮、以及
    // 消息正文的选中区 —— 见 `_reclaimFocus`
    return Listener(
      onPointerDown: _reclaimFocus,
      child: Focus(
        focusNode: _paneProbe,
        canRequestFocus: false,
        skipTraversal: true,
        child: _body(
          context,
          controller,
          sessionId,
          title,
          streaming,
          empty,
          composer,
        ),
      ),
    );
  }

  Widget _body(
    BuildContext context,
    ChatController controller,
    String? sessionId,
    String? title,
    bool streaming,
    bool empty,
    Widget Function({required bool centred}) composer,
  ) {
    final hasSession = sessionId != null;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: title ?? 'Cortex',
          // 不提「记忆」—— 长期记忆 2026-08-17 整条拆去了 Cormex，界面
          // 只许描述当下真的成立的能力（CLAUDE.md 约束 2）
          subtitle: hasSession ? null : '通用 AI Agent',
          // 左栏伸缩，**任何宽度都在**。此前它只在窄屏出现（那时叫
          // 「会话列表」），于是宽屏下根本没有收起侧栏这回事
          leading: widget.onToggleSessions == null
              ? null
              : IconButton(
                  onPressed: widget.onToggleSessions,
                  iconSize: 19,
                  tooltip: widget.sessionsVisible ? '隐藏会话栏' : '显示会话栏',
                  icon: Icon(
                    widget.sessionsVisible
                        ? Icons.menu_open_rounded
                        : Icons.menu_rounded,
                  ),
                ),
          actions: [
            // 第四状态在顶栏的落点：**哪几条会话在等你**。
            // 会话行上那个琥珀点只有翻到那一行才看得见，这颗 pill 保证
            // 「有人在等」这件事在任何滚动位置都可见，点一下直达
            const AwaitingConfirmPill(),
            // 工作区那个 chip 搬去了输入框底下（见 `MessageComposer` 里的
            // `_BeforeSendChips`）：它与权限档是同一类东西 —— 发出去**之前**
            // 要定的事。这一排剩下的都是应用级的显示开关，混在一起会让人
            // 以为工作区也是「看不看」而不是「跑在哪」
            const SyncIndicator(),
            const _BackendBadge(),
            // 本会话改动。右栏能开时开到「本轮改动」页签 —— 改动是
            // **对照着对话读**的，弹层会把对话挡住；右栏开不了的场景
            // 退回弹层
            if (hasSession)
              IconButton(
                onPressed: () {
                  final open = widget.onOpenPanel;
                  if (open == null) {
                    showSessionChanges(context);
                    return;
                  }
                  ref.read(railTabProvider.notifier).go(RailTab.changes);
                  open(RightPanel.files);
                },
                iconSize: 18,
                tooltip: '本会话改动',
                icon: const Icon(Icons.difference_outlined),
              ),
            // 设置搬去了左下角账号菜单（与 Codex 一致）：它与「我是谁」
            // 是一组，而这一行剩下的都是应用级的显示开关。
            //
            // 主题切换 2026-08-25 从这一排撤了：一个循环三档的图标按钮
            // 既看不出当前是哪档、又常年占着顶栏 —— 而主题是设一次就
            // 不再碰的偏好，不配一个逐轮可见的位置。入口在设置页「外观」
            // 与 ⌘K 命令面板，两处都还在
            // 右栏那两个，放在最右 —— 它们挨着的就是它们控制的那一栏。
            //
            // 图标从「文件夹」换成了侧栏折叠（与左栏那对 menu_open 同一族）：
            // 右栏已经是三页签（文件 / 本轮改动 / 终端），文件夹只说得出
            // 三分之一。图标常量在 right_rail.dart —— 栏头「收起」用的必须
            // 是同一个，见那里的注释
            if (widget.onSelectPanel != null) ...[
              _PanelButton(
                panel: RightPanel.files,
                active: widget.activePanel == RightPanel.files,
                tooltip: '右栏',
                filled: kRailToggleFilled,
                outlined: kRailToggleOutlined,
                onSelect: widget.onSelectPanel!,
              ),
            ],
          ],
        ),
        // 还没开口：输入框站在页面中央，上面是那块招呼，下面是几个起手式。
        // 这是 WorkBuddy / ChatGPT / Claude 都在用的形状，理由是同一个 ——
        // 空会话里输入框就是全部内容，把它钉在底边等于让用户隔着一整屏
        // 留白去够它。
        //
        // ⚠️ **判据里不能再有 `hasSession`。** 会话惰性化之后（点「新对话」
        // 不再立刻建会话），白纸上 `activeSessionId` 是 null，于是那一版
        // 会走进下面的钉底分支 —— 用户看到的是：输入框在底部，敲完回车
        // 会话兑现的一瞬间跳到居中，然后立刻因为有了消息又跳回底部。
        // **一次发送闪两下**，而中间那一帧还是空的。
        //
        // 现在两种「还没开口」共用同一条路：白纸、以及选中了一条还没说过
        // 话的会话。它们在用户眼里本来就是同一件事
        if (empty)
          Expanded(
            child: SingleChildScrollView(
              child: Center(
                child: Padding(
                  padding: const EdgeInsets.symmetric(vertical: 32),
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      const ConversationHero(),
                      const SizedBox(height: 22),
                      composer(centred: true),
                      const SizedBox(height: 18),
                      const ConversationPrompts(),
                    ],
                  ),
                ),
              ),
            ),
          )
        else ...[
          // 走到这儿必定有会话：`empty` 为假意味着有轮次或者正在流，
          // 两者都得先有一条会话才谈得上
          const Expanded(child: ConversationView()),
          composer(centred: false),
        ],
      ],
    );
  }
}

/// Tells the user at a glance whether they are looking at real data.
class _BackendBadge extends ConsumerWidget {
  const _BackendBadge();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final config = ref.watch(appConfigProvider);
    final health = ref.watch(healthProvider);

    final (Color color, String label, String tooltip)? badge = switch ((
      config.useMock,
      health,
    )) {
      (true, _) => (scheme.onSurfaceVariant, 'MOCK', '数据来自内存夹具，未连接后端'),
      // LIVE 且一切正常：**整个不画**。一枚恒真的常驻绿章只会训练人忽略
      // 那个位置 —— 等它哪天真变成 DOWN，那里早已是盲区。连接详情在
      // 设置的连接页里查得到，不靠这颗 tooltip
      (false, AsyncData(:final value))
          when value.isHealthy && value.databaseNote == null =>
        null,
      // 服务答 ok 但存储层有话要说（isHealthy 故意不看 database，见
      // health_status.dart）：这一种**不能跟着藏** —— 藏了就是
      // 「status: ok 盖住 database: error」那个见过的假信号
      (false, AsyncData(:final value)) when value.isHealthy => (
        theme.cortex.success,
        'LIVE',
        [config.baseUrl, 'v${value.version}', value.databaseNote!].join(' · '),
      ),
      (false, AsyncError()) => (scheme.error, 'DOWN', '连不上 ${config.baseUrl}'),
      _ => (scheme.onSurfaceVariant, '…', '正在检测 ${config.baseUrl}'),
    };
    if (badge == null) return const SizedBox.shrink();
    final (color, label, tooltip) = badge;

    return Tooltip(
      message: tooltip,
      child: Container(
        margin: const EdgeInsets.symmetric(horizontal: 4),
        padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 3),
        decoration: BoxDecoration(
          color: color.withValues(alpha: 0.12),
          borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
          border: Border.all(color: color.withValues(alpha: 0.35)),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Container(
              width: 5,
              height: 5,
              decoration: BoxDecoration(color: color, shape: BoxShape.circle),
            ),
            const SizedBox(width: 5),
            Text(
              label,
              style: TextStyle(
                fontSize: 10,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.6,
                color: color,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// 离线模式期间**一直**挂着的一条提示。
///
/// # 为什么不是「入口说一次就够了」
///
/// 这个模式唯一的代价是「没有记忆」，而记忆恰恰是这个产品的全部卖点。
/// 一个在入口点了「离线使用」的人，二十分钟后正在专心干活时，
/// 完全可能忘了自己处在一个不记事的状态里 —— 然后对着一句
/// 「我上次说的那个方案」发现它什么都不知道。
///
/// 所以这条不可关闭：它不是通知，是**当前状态**。
class _OfflineBanner extends ConsumerWidget {
  const _OfflineBanner();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final offline = ref.watch(appConfigProvider.select((c) => c.offline));
    if (!offline) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final tokens = theme.cortex;
    return Container(
      width: double.infinity,
      // 琥珀软垫。⚠️ 不用 tertiaryContainer —— 这套主题没定义 tertiary，
      // M3 会静默回落成 teal，把「离线在攒队列」这句警示画成安心色
      color: tokens.warning.withValues(alpha: 0.12),
      padding: const EdgeInsets.fromLTRB(20, 8, 10, 8),
      child: Row(
        children: [
          Icon(Icons.cloud_off_outlined, size: 16, color: tokens.warning),
          const SizedBox(width: 9),
          Expanded(
            // 同上：Text 不渲染 Markdown，星号会原样显示
            child: Text.rich(
              TextSpan(
                // 正文跟表面走（onSurface）：软垫只有 12% 透明度，底下仍是
                // 内容区。琥珀只留给图标与强调字 —— 整段染黄反而分不出重点
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onSurface,
                ),
                children: [
                  const TextSpan(text: '离线模式：这些对话'),
                  TextSpan(
                    text: '还没进服务端',
                    style: TextStyle(
                      fontWeight: FontWeight.w700,
                      color: tokens.warning,
                    ),
                  ),
                  // 「看不到以前的会话」必须在这儿说：离线时 `list_sessions`
                  // 是纯转发，列表只剩本次的草稿 —— 不说的话用户读到的是
                  // 「我昨天那些对话没了」。这句从前只写在登录页的离线卡上，
                  // 而真正对着空列表发慌的人已经过了登录页
                  const TextSpan(
                    text: '。它们排在本地队列，接上服务器后自动补回去；以前的会话也在服务器上，接上就回来。',
                  ),
                ],
              ),
            ),
          ),
          TextButton(
            onPressed: () =>
                ref.read(appConfigProvider.notifier).setOffline(false),
            child: const Text('去连接'),
          ),
        ],
      ),
    );
  }
}

class _SendErrorBanner extends ConsumerWidget {
  const _SendErrorBanner();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final error = ref.watch(chatControllerProvider.select((s) => s.sendError));
    if (error == null) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final controller = ref.read(chatControllerProvider.notifier);

    return Container(
      width: double.infinity,
      color: scheme.errorContainer,
      padding: const EdgeInsets.fromLTRB(20, 10, 10, 10),
      child: Row(
        children: [
          Icon(Icons.error_outline_rounded, size: 16, color: scheme.error),
          const SizedBox(width: 9),
          Expanded(
            child: Text(
              error,
              maxLines: 3,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onErrorContainer,
              ),
            ),
          ),
          TextButton(onPressed: controller.retryLast, child: const Text('重试')),
          // 模型失败的第二条出路：**换一个再试**。失败往往是这个模型的事
          // （配额、超时、这家供应商挂了），换一个立刻能走 —— 只给「重试」
          // 等于让人对着同一堵墙撞第二次
          TextButton(
            onPressed: () => showModelPicker(context, ref),
            child: const Text('换模型'),
          ),
          IconButton(
            onPressed: controller.clearSendError,
            iconSize: 16,
            icon: const Icon(Icons.close_rounded),
          ),
        ],
      ),
    );
  }
}

/// 右栏那两个图标共用的一个。
///
/// 提出来是因为它们除了图标与文案**完全一样** —— 而两份复制粘贴的
/// IconButton 里，迟早有一份会漏掉「选中态实心」或者 tooltip 忘了改。
class _PanelButton extends StatelessWidget {
  const _PanelButton({
    required this.panel,
    required this.active,
    required this.tooltip,
    required this.filled,
    required this.outlined,
    required this.onSelect,
  });

  final RightPanel panel;
  final bool active;
  final String tooltip;
  final IconData filled;
  final IconData outlined;
  final void Function(RightPanel) onSelect;

  @override
  Widget build(BuildContext context) {
    return IconButton(
      onPressed: () => onSelect(panel),
      iconSize: 19,
      // 开着的时候说「收起」：同一个按钮既是「给我看」也是「不看了」，
      // 用户不用去找第二个关闭入口。措辞与栏头那个「收起右栏」一致 ——
      // 它们做的是同一件事
      tooltip: active ? '收起$tooltip' : '展开$tooltip',
      // 激活态只靠 filled/outlined 图标切换，不上色 —— 「选中」在这个
      // 产品里一律用中性表达，上一版的 teal 会被读成某个含义不明的状态
      icon: Icon(active ? filled : outlined),
    );
  }
}
