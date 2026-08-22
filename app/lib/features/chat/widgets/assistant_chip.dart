/// 输入框底部那个「这条对话用哪个智能体」。
///
/// # 它与旁边三个 chip **不是**同一类
///
/// 工作区 / 权限档 / 模型都是**逐轮**的决定：这一句用哪个，下一句能换。
/// 人设不是 —— 它是系统提示词的第一段，也就是可缓存前缀的头
/// （CLAUDE.md 约束 4）。中途换掉有两个后果，而且都不会报错：
///
/// 1. 从那一轮起每一轮都在打穿 prompt caching（这套系统里最贵的一样东西）；
/// 2. 历史里模型已经以旧身份说过话了，接下来它以新身份接着说 ——
///    整段对话前后不一致，而用户只会觉得「它忘了自己是谁」。
///
/// 所以这个 chip 的规矩是：**这条对话还一个字都没说时可以随便挑，
/// 说过话之后就只显示、不再改**。想换？开一条新的。
///
/// 「已经开始了就锁住」这件事必须**看得见**：只是把它禁用掉的话，用户会以为
/// 界面卡了。所以锁住时 tooltip 直说为什么，点它给一条出路（用它开一条新的）。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../models/assistant.dart';
import '../../../state/assistant_controller.dart';
import '../../../state/chat_controller.dart';

class AssistantChip extends ConsumerWidget {
  const AssistantChip({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    final state = ref.watch(assistantControllerProvider);
    // 老服务端没有 `/assistants`。一个点开只会说「选不了」的 chip 是纯噪音，
    // 与 `ModelChip` 同一个判断
    if (state.unsupported) return const SizedBox.shrink();

    final sessionId = ref.watch(
      chatControllerProvider.select((s) => s.activeSessionId),
    );
    final started = ref.watch(
      chatControllerProvider.select((s) => s.activeTranscript.isNotEmpty),
    );
    final current = state.byId(
      ref.watch(sessionAssistantProvider.select((m) => m[sessionId])),
    );

    // 一个字都没说、也没挑过 —— 那就还没有「智能体」这个话题。
    // 空对话上摆一个「默认助理 ⌃」是在给一个大多数人不需要的概念占位置；
    // 想用的人从左栏那一页进来（点卡片 = 用它开一条新的）
    if (current == null && (started || state.assistants.isEmpty)) {
      return const SizedBox.shrink();
    }

    final locked = started && current != null;
    final color = locked
        ? theme.cortex.foregroundTertiary
        : scheme.onSurfaceVariant;

    return Tooltip(
      message: locked
          ? '这条对话正在用「${current.name}」。人设开始之后就不再换 —— '
                '换了的话它前面说过的话就不是它说的了。点一下用它开一条新的'
          : '这条对话用哪个智能体',
      child: InkWell(
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        onTap: () => locked
            ? _startFresh(ref, current)
            : _pick(context, ref, state.assistants, sessionId, current),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 3),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (current != null && current.icon.isNotEmpty)
                Text(current.icon, style: const TextStyle(fontSize: 12))
              else
                Icon(Icons.smart_toy_outlined, size: 13, color: color),
              const SizedBox(width: 4),
              ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 120),
                child: Text(
                  current?.name ?? '默认助理',
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.labelSmall?.copyWith(color: color),
                ),
              ),
              Icon(
                locked
                    ? Icons.lock_outline_rounded
                    : Icons.arrow_drop_up_rounded,
                size: locked ? 11 : 15,
                color: color,
              ),
            ],
          ),
        ),
      ),
    );
  }

  void _startFresh(WidgetRef ref, Assistant a) {
    final id = ref.read(chatControllerProvider.notifier).createSession();
    ref.read(sessionAssistantProvider.notifier).bind(id, a.id);
  }

  Future<void> _pick(
    BuildContext context,
    WidgetRef ref,
    List<Assistant> all,
    String? sessionId,
    Assistant? current,
  ) async {
    final picked = await showModalBottomSheet<String?>(
      context: context,
      showDragHandle: true,
      builder: (ctx) => SafeArea(
        child: ListView(
          shrinkWrap: true,
          children: [
            ListTile(
              leading: const Icon(Icons.smart_toy_outlined, size: 20),
              title: const Text('默认助理'),
              subtitle: const Text('通用 AI 助理，没有额外人设'),
              selected: current == null,
              // ⚠️ 空串而不是 null：`pop(null)` 与「用户点了外面关掉」
              // 在返回值上完全一样，那样选「默认助理」会变成什么都没发生
              onTap: () => Navigator.of(ctx).pop(''),
            ),
            for (final a in all)
              ListTile(
                leading: Text(
                  a.icon.isEmpty ? '🤖' : a.icon,
                  style: const TextStyle(fontSize: 18),
                ),
                title: Text(a.name),
                subtitle: a.description.isEmpty
                    ? null
                    : Text(
                        a.description,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                selected: a.id == current?.id,
                onTap: () => Navigator.of(ctx).pop(a.id),
              ),
          ],
        ),
      ),
    );
    if (picked == null) return;
    // 还没有会话就先建一条：绑定要有个东西可绑
    final id =
        sessionId ?? ref.read(chatControllerProvider.notifier).createSession();
    ref
        .read(sessionAssistantProvider.notifier)
        .bind(id, picked.isEmpty ? null : picked);
  }
}
