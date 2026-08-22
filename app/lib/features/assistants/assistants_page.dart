/// 智能体 —— 一面卡片墙。
///
/// 别家各叫各的：专家（workbuddy）、智能体小助手（Cherry Studio）、
/// 助理（LobeHub）、搭档（chatbox）。是同一个东西：一组存下来的人设，
/// 开对话时挑一个。
///
/// # 骨架逐字照着项目页与图片页
///
/// `PanelHeader` + `SliverGrid`。三个「地方」长得不一样的话，用户每进一个
/// 都要重新学一遍它的顶栏在哪儿。
///
/// # ⚠️ 点卡片 = **用它开一条新对话**，不是「切换当前会话的人设」
///
/// 系统提示词是可缓存前缀的第一段（CLAUDE.md 约束 4）。在一条会话中途换
/// 人设，等于从那一轮起每一轮都打穿 prompt caching；而且历史里模型已经
/// 用旧人设说过话了，中途换掉会让整段对话前后不一致。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/theme.dart';
import '../../models/assistant.dart';
import '../../state/app_providers.dart';
import '../../state/assistant_controller.dart';
import '../../state/chat_controller.dart';
import '../../state/project_controller.dart';
import '../../widgets/empty_state.dart';
import '../../widgets/panel_header.dart';
import 'assistant_editor.dart';

class AssistantsPage extends ConsumerWidget {
  const AssistantsPage({
    super.key,
    this.onToggleSessions,
    this.sessionsVisible = false,
  });

  final VoidCallback? onToggleSessions;
  final bool sessionsVisible;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final state = ref.watch(assistantControllerProvider);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: '智能体',
          subtitle: state.assistants.isEmpty ? null : '点一个，用它开一条新对话',
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
            if (!state.unsupported)
              TextButton.icon(
                key: const ValueKey('assistants:new'),
                onPressed: () => _edit(context, ref, null),
                icon: const Icon(Icons.add_rounded, size: 16),
                label: const Text('新建智能体'),
              ),
          ],
        ),
        Expanded(child: _body(context, ref, state)),
      ],
    );
  }

  Widget _body(BuildContext context, WidgetRef ref, AssistantState state) {
    if (state.unsupported) {
      return const EmptyState(
        icon: Icons.smart_toy_outlined,
        title: '这个后端没有智能体',
        // 说清是**后端**旧了，不是出错了 —— 重试永远不会成功。
        // 与画廊、项目那两页的措辞刻意一致
        description: '它是一个老版本的部署，还没有 /assistants 这条路。升级之后这里会自己出现。',
      );
    }
    if (state.error != null) {
      return EmptyState(
        icon: Icons.cloud_off_rounded,
        title: '拉不到智能体',
        description: '${state.error}',
        tone: EmptyStateTone.error,
        action: OutlinedButton.icon(
          onPressed: () =>
              ref.read(assistantControllerProvider.notifier).refresh(),
          icon: const Icon(Icons.refresh_rounded, size: 16),
          label: const Text('重试'),
        ),
      );
    }
    if (state.loading && state.assistants.isEmpty) {
      return const Center(
        child: SizedBox(
          width: 20,
          height: 20,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      );
    }
    if (state.assistants.isEmpty) {
      return EmptyState(
        icon: Icons.smart_toy_outlined,
        title: '还没有智能体',
        description:
            '一个智能体就是一份写好的人设 —— 「一位资深大厨」「一位严格的代码审查者」。'
            '开对话时挑一个，它就按那个身份说话。',
        action: FilledButton.icon(
          onPressed: () => _edit(context, ref, null),
          icon: const Icon(Icons.add_rounded, size: 18),
          label: const Text('新建智能体'),
        ),
      );
    }

    return CustomScrollView(
      slivers: [
        SliverPadding(
          padding: const EdgeInsets.fromLTRB(20, 16, 20, 24),
          // 与图库同一个理由：`shrinkWrap` 会把整页卡片全建出来
          sliver: SliverGrid.builder(
            gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
              maxCrossAxisExtent: 420,
              crossAxisSpacing: 12,
              mainAxisSpacing: 12,
              mainAxisExtent: 132,
            ),
            itemCount: state.assistants.length,
            itemBuilder: (context, i) => _AssistantCard(
              key: ValueKey('assistant:${state.assistants[i].id}'),
              assistant: state.assistants[i],
            ),
          ),
        ),
      ],
    );
  }
}

Future<void> _edit(BuildContext context, WidgetRef ref, Assistant? existing) =>
    showAssistantEditor(context, ref, existing);

class _AssistantCard extends ConsumerWidget {
  const _AssistantCard({super.key, required this.assistant});

  final Assistant assistant;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Material(
      color: scheme.surfaceContainerLow,
      clipBehavior: Clip.antiAlias,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        side: BorderSide(color: scheme.outlineVariant),
      ),
      child: InkWell(
        key: ValueKey('assistant:use:${assistant.id}'),
        // ⚠️ **用它开一条新对话**，不是切换当前会话的人设。
        // 中途换人设会打穿 prompt caching，而且历史里模型已经用旧人设
        // 说过话了 —— 换掉之后整段对话前后不一致
        onTap: () => startChatWith(context, ref, assistant),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 6, 12),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Text(
                    // 没填图标就给一个 —— 一排空白读起来像加载失败
                    assistant.icon.isEmpty ? '🤖' : assistant.icon,
                    style: const TextStyle(fontSize: 18),
                  ),
                  const SizedBox(width: 8),
                  Flexible(
                    child: Text(
                      assistant.name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.titleSmall,
                    ),
                  ),
                  const Spacer(),
                  _AssistantMenu(assistant: assistant),
                ],
              ),
              const SizedBox(height: 6),
              Expanded(
                child: Text(
                  // 没写说明就露一段人设本身：比一片空白有用，
                  // 而且正好提醒用户「这里可以写一句话说明」
                  assistant.description.isNotEmpty
                      ? assistant.description
                      : assistant.instructions,
                  maxLines: 3,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.cortex.foregroundTertiary,
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 用这个智能体开一条新对话。
///
/// 三步缺一不可：建会话 → 把会话与智能体绑上 → 回到聊天页。
/// 少了第二步，模型仍然用默认人设，而用户以为自己选过了。
void startChatWith(BuildContext context, WidgetRef ref, Assistant assistant) {
  final id = ref.read(chatControllerProvider.notifier).createSession();
  ref.read(sessionAssistantProvider.notifier).bind(id, assistant.id);
  ref.read(projectControllerProvider.notifier).select(null);
  ref.read(mainViewProvider.notifier).go(MainView.chat);
}

class _AssistantMenu extends ConsumerWidget {
  const _AssistantMenu({required this.assistant});

  final Assistant assistant;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return PopupMenuButton<String>(
      key: ValueKey('assistant:menu:${assistant.id}'),
      tooltip: '「${assistant.name}」的更多操作',
      iconSize: 16,
      icon: const Icon(Icons.more_vert_rounded),
      onSelected: (value) => switch (value) {
        'edit' => showAssistantEditor(context, ref, assistant),
        _ => _confirmDelete(context, ref, assistant),
      },
      itemBuilder: (context) => const [
        PopupMenuItem(
          value: 'edit',
          height: 38,
          child: Row(
            children: [
              Icon(Icons.edit_outlined, size: 15),
              SizedBox(width: 9),
              Text('编辑'),
            ],
          ),
        ),
        PopupMenuItem(
          value: 'delete',
          height: 38,
          child: Row(
            children: [
              Icon(Icons.delete_outline_rounded, size: 15),
              SizedBox(width: 9),
              Text('删除'),
            ],
          ),
        ),
      ],
    );
  }

  Future<void> _confirmDelete(
    BuildContext context,
    WidgetRef ref,
    Assistant a,
  ) async {
    final messenger = ScaffoldMessenger.maybeOf(context);
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text('删除「${a.name}」？'),
        // ⚠️ **必须说清历史不会变。** 用户在这里唯一真正害怕的事就是
        // 「删了人设，之前那些对话是不是就坏了」—— 而那恰恰不会发生：
        // 人设是逐轮带的，消息早就落库了
        content: const Text('用它聊过的那些对话一条都不会变 —— 人设是发出去那一刻带上的，不是存在对话里的。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(ctx).pop(true),
            child: const Text('删除'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    try {
      await ref.read(assistantControllerProvider.notifier).remove(a.id);
    } on Object catch (e) {
      messenger?.showSnackBar(SnackBar(content: Text('$e')));
    }
  }
}
