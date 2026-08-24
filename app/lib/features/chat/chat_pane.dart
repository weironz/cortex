import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';
import '../../widgets/panel_header.dart';
import '../settings/pages/model_picker.dart';
import '../shell/widgets/sync_indicator.dart';
import 'session_changes_sheet.dart';
import 'widgets/awaiting_confirm_pill.dart';
import 'widgets/confirm_panel.dart';
import 'widgets/conversation_view.dart';
import 'widgets/message_composer.dart';
import '../../core/theme.dart';

/// Centre pane: header, transcript, composer.
class ChatPane extends ConsumerWidget {
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
  Widget build(BuildContext context, WidgetRef ref) {
    final controller = ref.read(chatControllerProvider.notifier);
    final sessionId = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    final hasSession = sessionId != null;
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
          onSend: (text, attachments) =>
              controller.send(text, attachments: attachments),
          onStop: controller.stopGeneration,
        ),
      ],
    );

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: title ?? 'Cortex',
          subtitle: hasSession ? null : '记忆原生的通用 AI Agent',
          // 左栏伸缩，**任何宽度都在**。此前它只在窄屏出现（那时叫
          // 「会话列表」），于是宽屏下根本没有收起侧栏这回事
          leading: onToggleSessions == null
              ? null
              : IconButton(
                  onPressed: onToggleSessions,
                  iconSize: 19,
                  tooltip: sessionsVisible ? '隐藏会话栏' : '显示会话栏',
                  icon: Icon(
                    sessionsVisible
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
                  final open = onOpenPanel;
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
            // 是一组，而这一行剩下的都是应用级的显示开关
            IconButton(
              onPressed: () => ref.read(themeModeProvider.notifier).cycle(),
              iconSize: 18,
              tooltip: '切换主题',
              icon: Icon(switch (ref.watch(themeModeProvider)) {
                ThemeMode.system => Icons.brightness_auto_rounded,
                ThemeMode.light => Icons.light_mode_rounded,
                ThemeMode.dark => Icons.dark_mode_rounded,
              }),
            ),
            // 右栏那两个，放在最右 —— 它们挨着的就是它们控制的那一栏。
            //
            // **文件在前、记忆在后**：记忆紧贴右栏边缘，与它此前独占这个
            // 位置时一致；对已经形成肌肉记忆的人，那个图标没有挪窝
            if (onSelectPanel != null) ...[
              _PanelButton(
                panel: RightPanel.files,
                active: activePanel == RightPanel.files,
                tooltip: '文件',
                filled: Icons.folder_rounded,
                outlined: Icons.folder_outlined,
                onSelect: onSelectPanel!,
              ),
            ],
          ],
        ),
        // 还没开口的会话：输入框站在页面中央，上面是那块招呼，下面是几个
        // 起手式。这是 WorkBuddy / ChatGPT / Claude 都在用的形状，理由是
        // 同一个 —— 空会话里输入框就是全部内容，把它钉在底边等于让用户
        // 隔着一整屏留白去够它。
        //
        // **只有选中了会话才走这一支**。没选中时照旧「提示 + 底部输入框」：
        // 那一版里输入框是禁用的，但 `ConfirmPanel` 跟着它一起挂在树上，
        // 而它一挂上就去捞待办确认。不渲染的话那次捞晚发生，
        // 测试里表现为「树都销毁了还有定时器没停」
        if (hasSession && empty)
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
          Expanded(
            child: hasSession
                ? const ConversationView()
                : const NoSessionState(),
          ),
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

    final (Color color, String label, String tooltip) = switch ((
      config.useMock,
      health,
    )) {
      (true, _) => (scheme.onSurfaceVariant, 'MOCK', '数据来自内存夹具，未连接后端'),
      (false, AsyncData(:final value)) when value.isHealthy => (
        const Color(0xFF2E9E5B),
        'LIVE',
        [
          config.baseUrl,
          'v${value.version}',
          if (value.databaseNote != null) value.databaseNote!,
        ].join(' · '),
      ),
      (false, AsyncError()) => (scheme.error, 'DOWN', '连不上 ${config.baseUrl}'),
      _ => (scheme.onSurfaceVariant, '…', '正在检测 ${config.baseUrl}'),
    };

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
    return Container(
      width: double.infinity,
      color: scheme.tertiaryContainer,
      padding: const EdgeInsets.fromLTRB(20, 8, 10, 8),
      child: Row(
        children: [
          Icon(
            Icons.cloud_off_outlined,
            size: 16,
            color: scheme.onTertiaryContainer,
          ),
          const SizedBox(width: 9),
          Expanded(
            // 同上：Text 不渲染 Markdown，星号会原样显示
            child: Text.rich(
              TextSpan(
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onTertiaryContainer,
                ),
                children: const [
                  TextSpan(text: '离线模式：这些对话'),
                  TextSpan(
                    text: '还没进服务端',
                    style: TextStyle(fontWeight: FontWeight.w700),
                  ),
                  TextSpan(text: '。它们排在本地队列，接上服务器后会自动补回去。'),
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
      // 开着的时候说「隐藏」：同一个按钮既是「给我看」也是「不看了」，
      // 用户不用去找第二个关闭入口
      tooltip: active ? '隐藏$tooltip栏' : tooltip,
      color: active ? Theme.of(context).colorScheme.secondary : null,
      icon: Icon(active ? filled : outlined),
    );
  }
}
