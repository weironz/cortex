import 'package:flutter/material.dart';
import '../../core/theme.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../models/chat_message.dart';
import '../../models/tool_call.dart';
import '../../state/chat_controller.dart';
import 'widgets/diff_view.dart';

/// 「本会话改动」—— 这一整轮对话动过哪些文件，各改了什么。
///
/// # 为什么是弹层，不是第三个侧栏
///
/// 右边已经是记忆栏，而记忆是这个产品的主张，不该为了改动把它降级成一个
/// 页签。而且「改动」是**回顾性**的：你是想起来才去看，不是一直盯着 ——
/// 常驻一栏给它，是把偶尔的需要摆成了持续的占地。
Future<void> showSessionChanges(BuildContext context) =>
    showDialog<void>(context: context, builder: (_) => const _ChangesDialog());

/// 一个文件在本会话里的全部改动。
class FileChanges {
  const FileChanges({required this.path, required this.diffs});

  final String path;

  /// 按发生顺序。**不合并** —— 同一个文件被改了三次就是三段，
  /// 合并成一份「最终 diff」会把「它改了三次」这个事实抹掉，
  /// 而那正是回头看时最想知道的事情之一。
  final List<String> diffs;

  ({int added, int removed}) get totals {
    var a = 0;
    var r = 0;
    for (final d in diffs) {
      final c = countChanges(d);
      a += c.added;
      r += c.removed;
    }
    return (added: a, removed: r);
  }
}

/// 把一串消息里的工具调用汇总成「按文件」的改动。
///
/// 纯函数，好测。汇总口径要与人的直觉一致：
///
/// - 只收**有 diff 的**调用。没有 diff 的（读文件、shell、记忆检索）不是改动
/// - 按 `path` 分组，**保持首次出现的顺序**（不排序）：人记得的是
///   「先改了 A 再改了 B」，按字母重排会让这个顺序消失
List<FileChanges> groupChangesByFile(List<ChatMessage> messages) {
  final order = <String>[];
  final byPath = <String, List<String>>{};

  for (final m in messages) {
    for (final ToolCall c in m.toolCalls) {
      final diff = c.diff;
      if (diff == null || diff.isEmpty) continue;
      final path = c.path ?? c.name;
      if (!byPath.containsKey(path)) order.add(path);
      byPath.putIfAbsent(path, () => []).add(diff);
    }
  }

  return [
    for (final p in order) FileChanges(path: p, diffs: byPath[p] ?? const []),
  ];
}

/// 本会话改动的正文 —— 弹层与右栏「本轮改动」页签**共用这一份**。
///
/// 抄成两份的下场：空态文案、「改了 N 次」标注、diff 上限这些细节
/// 各自演化，哪天一边修了另一边还带着老毛病。
class SessionChangesView extends ConsumerWidget {
  const SessionChangesView({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final messages = ref.watch(
      chatControllerProvider.select((s) => s.activeTranscript),
    );
    final files = groupChangesByFile(messages);

    if (files.isEmpty) {
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 24, horizontal: 4),
        child: Text(
          // 说清是「没改过」而不是「没记录」—— 两者对用户的下一步
          // 完全不同：前者不用去查，后者要去查为什么没记上
          '这个会话还没有改过任何文件。',
          style: theme.textTheme.bodyMedium?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
        ),
      );
    }
    return SingleChildScrollView(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [for (final f in files) _FileSection(file: f)],
      ),
    );
  }
}

class _ChangesDialog extends ConsumerWidget {
  const _ChangesDialog();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final messages = ref.watch(
      chatControllerProvider.select((s) => s.activeTranscript),
    );
    final files = groupChangesByFile(messages);

    return AlertDialog(
      title: Row(
        children: [
          const Icon(Icons.difference_outlined, size: 19),
          const SizedBox(width: 8),
          const Text('本会话改动'),
          const Spacer(),
          if (files.isNotEmpty)
            Text(
              '${files.length} 个文件',
              style: theme.textTheme.labelMedium?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
            ),
        ],
      ),
      contentPadding: const EdgeInsets.fromLTRB(24, 8, 24, 0),
      content: const SizedBox(width: 720, child: SessionChangesView()),
      actions: [
        FilledButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('好'),
        ),
      ],
    );
  }
}

class _FileSection extends StatelessWidget {
  const _FileSection({required this.file});

  final FileChanges file;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final t = file.totals;

    return Padding(
      padding: const EdgeInsets.only(bottom: 16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(
                Icons.description_outlined,
                size: 14,
                color: scheme.onSurfaceVariant,
              ),
              const SizedBox(width: 6),
              Expanded(
                child: SelectionArea(
                  child: Text(
                    file.path,
                    style: theme.textTheme.bodySmall?.copyWith(
                      fontFamily: 'JetBrains Mono',
                      fontFamilyFallback: CortexTheme.monoFallback,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ),
              ),
              const SizedBox(width: 8),
              Text(
                '+${t.added} −${t.removed}',
                style: theme.textTheme.labelSmall?.copyWith(
                  fontFamily: 'JetBrains Mono',
                  fontFamilyFallback: CortexTheme.monoFallback,
                  color: scheme.onSurfaceVariant,
                ),
              ),
              // 同一个文件改了不止一次时说出来：合并显示会把这个事实抹掉
              if (file.diffs.length > 1) ...[
                const SizedBox(width: 8),
                Text(
                  '改了 ${file.diffs.length} 次',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: scheme.secondary,
                  ),
                ),
              ],
            ],
          ),
          const SizedBox(height: 6),
          for (final d in file.diffs)
            Padding(
              padding: const EdgeInsets.only(bottom: 6),
              child: DiffView(d, maxHeight: 320),
            ),
        ],
      ),
    );
  }
}
