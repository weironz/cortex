import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/formatting.dart';
import '../../models/attachment.dart';
import '../../models/import_plan.dart';
import '../../state/import_controller.dart';

Future<void> showImportSheet(BuildContext context) {
  return showDialog<void>(
    context: context,
    // The run takes many minutes. Tapping the scrim mid-import and losing the
    // progress bar would look like the import stopped — it would not have.
    barrierDismissible: false,
    builder: (_) => const _ImportDialog(),
  );
}

/// Import a ChatGPT / Claude export.
///
/// ## The one rule this screen exists to enforce
///
/// **The bill is shown before the button appears.** Every pair in the estimate
/// is one LLM call — a real Claude export is 6047 of them — and memory is
/// append-only, so an import cannot be undone by clicking something. Undoing it
/// means `redact`, which is deliberately explicit and confirmed twice.
///
/// The daemons encode the same rule structurally: preview is its own read-only
/// endpoint, not `run(dry: true)`. This screen must not be the place where that
/// care is thrown away.
class _ImportDialog extends ConsumerWidget {
  const _ImportDialog();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final state = ref.watch(importControllerProvider);
    final controller = ref.read(importControllerProvider.notifier);

    return AlertDialog(
      title: const Text('导入 ChatGPT / Claude 历史'),
      contentPadding: const EdgeInsets.fromLTRB(24, 16, 24, 0),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 480),
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '在 ChatGPT 的「设置 → 数据控制 → 导出数据」或 Claude 的'
                '「设置 → 隐私 → 导出数据」申请导出，邮件里会给下载链接。'
                '压缩包里的 conversations.json 就是这里要选的文件。',
                style: theme.textTheme.bodySmall,
              ),
              const SizedBox(height: 16),
              _FilePicked(state: state),
              if (state.estimate != null) ...[
                const SizedBox(height: 16),
                _Bill(estimate: state.estimate!, phase: state.phase),
              ],
              if (state.phase == ImportPhase.estimated) ...[
                const SizedBox(height: 12),
                _BatchPicker(
                  value: state.maxConversations,
                  onChanged: controller.setMaxConversations,
                ),
              ],
              if (state.progress != null) ...[
                const SizedBox(height: 16),
                _Progress(progress: state.progress!),
              ],
              if (state.done != null) ...[
                const SizedBox(height: 16),
                _Summary(done: state.done!),
              ],
              if (state.error != null) ...[
                const SizedBox(height: 16),
                _Problem(message: state.error!),
              ],
              const SizedBox(height: 8),
            ],
          ),
        ),
      ),
      actions: _actions(context, state, controller),
    );
  }

  List<Widget> _actions(
    BuildContext context,
    ImportState state,
    ImportController controller,
  ) {
    switch (state.phase) {
      case ImportPhase.idle:
        return [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: controller.pickAndPreview,
            child: const Text('选择文件'),
          ),
        ];
      case ImportPhase.preparing:
        return const [TextButton(onPressed: null, child: Text('读取中…'))];
      case ImportPhase.estimated:
        return [
          TextButton(onPressed: controller.reset, child: const Text('换一个文件')),
          // Only here, and only after the numbers above are on screen.
          FilledButton(onPressed: controller.start, child: const Text('开始导入')),
        ];
      case ImportPhase.running:
        return [
          TextButton(onPressed: controller.cancel, child: const Text('停止跟踪')),
        ];
      case ImportPhase.finished:
        return [
          TextButton(onPressed: controller.reset, child: const Text('再导一个')),
          FilledButton(
            onPressed: () {
              controller.reset();
              Navigator.of(context).pop();
            },
            child: const Text('完成'),
          ),
        ];
    }
  }
}

class _FilePicked extends StatelessWidget {
  const _FilePicked({required this.state});

  final ImportState state;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final source = state.source;
    if (source == null) {
      return Text('还没有选择文件。', style: theme.textTheme.bodyMedium);
    }
    return Row(
      children: [
        const Icon(Icons.description_outlined, size: 18),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            '${source.filename} · ${formatBytes(source.sizeBytes)}',
            style: theme.textTheme.bodyMedium,
            overflow: TextOverflow.ellipsis,
          ),
        ),
        if (state.phase == ImportPhase.preparing)
          const SizedBox(
            width: 14,
            height: 14,
            child: CircularProgressIndicator(strokeWidth: 2),
          ),
      ],
    );
  }
}

/// The numbers the user is being asked to approve.
class _Bill extends StatelessWidget {
  const _Bill({required this.estimate, required this.phase});

  final ImportEstimate estimate;
  final ImportPhase phase;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('来源：${estimate.platform}', style: theme.textTheme.titleSmall),
          const SizedBox(height: 8),
          _row(theme, '对话', '${estimate.conversations} 段'),
          _row(theme, '消息', '${estimate.messages} 条'),
          if (estimate.earliest != null && estimate.latest != null)
            _row(
              theme,
              '时间跨度',
              '${formatDate(estimate.earliest)} → ${formatDate(estimate.latest)}',
            ),
          const Divider(height: 20),
          // The line that matters. Bold because it is the one the user is
          // actually approving — everything above is context for it.
          _row(theme, '会触发抽取', '${estimate.pairs} 次', emphasis: true),
          _row(theme, '送进抽取的文本', '约 ${formatThousands(estimate.tokens)} token'),
          _row(theme, '预计耗时', '至少 ${estimate.minutes.toStringAsFixed(0)} 分钟'),
          const SizedBox(height: 8),
          Text(
            '每次抽取是一回 LLM 调用，按你那个模型的单价自己乘。'
            '导入之后没有「撤销」按钮 —— 会话是只追加的，要撤得逐条删。',
            style: theme.textTheme.bodySmall?.copyWith(
              color: scheme.onSurfaceVariant,
            ),
          ),
          if (estimate.unpaired > 0) ...[
            const SizedBox(height: 8),
            Text(
              '另有 ${estimate.unpaired} 条消息落单（开头就是助手、或最后一问没有回答）。'
              '它们照样写进原文，只是不产生事实 —— 所以「导入了多少条」'
              '会比「多出多少条消息」大一些。',
              style: theme.textTheme.bodySmall?.copyWith(
                color: scheme.onSurfaceVariant,
              ),
            ),
          ],
        ],
      ),
    );
  }

  Widget _row(
    ThemeData theme,
    String label,
    String value, {
    bool emphasis = false,
  }) {
    final style = emphasis
        ? theme.textTheme.bodyMedium?.copyWith(fontWeight: FontWeight.w700)
        : theme.textTheme.bodyMedium;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.spaceBetween,
        children: [
          Text(label, style: theme.textTheme.bodySmall),
          Text(value, style: style),
        ],
      ),
    );
  }
}

/// "Only the newest N conversations."
///
/// Exists so trying it out is cheap. Without it the first run is all-or-nothing
/// over three years of history, and the way to find out whether the result is
/// any good would be to pay for the whole thing first.
class _BatchPicker extends StatelessWidget {
  const _BatchPicker({required this.value, required this.onChanged});

  final int? value;
  final ValueChanged<int?> onChanged;

  static const _options = <int?, String>{
    5: '最近 5 段（试水）',
    50: '最近 50 段',
    null: '全部',
  };

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        Text('导入范围', style: Theme.of(context).textTheme.bodySmall),
        const SizedBox(width: 12),
        Expanded(
          child: SegmentedButton<int?>(
            segments: [
              for (final e in _options.entries)
                ButtonSegment(value: e.key, label: Text(e.value)),
            ],
            selected: {value},
            showSelectedIcon: false,
            onSelectionChanged: (s) => onChanged(s.first),
          ),
        ),
      ],
    );
  }
}

class _Progress extends StatelessWidget {
  const _Progress({required this.progress});

  final ImportProgressEvent progress;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final total = progress.conversationsTotal;
    final ratio = total == 0 ? 0.0 : progress.conversationsDone / total;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        LinearProgressIndicator(value: ratio.clamp(0.0, 1.0)),
        const SizedBox(height: 8),
        Text(
          '${progress.conversationsDone} / $total 段 · '
          '${progress.pairsDone} 对已灌入',
          style: theme.textTheme.bodySmall,
        ),
        if (progress.skipped > 0)
          Text(
            '其中 ${progress.skipped} 条此前已导过，服务端直接跳过（没有重复计费）',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
      ],
    );
  }
}

class _Summary extends StatelessWidget {
  const _Summary({required this.done});

  final ImportDoneEvent done;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('导入完成：${done.pairsDone} 对', style: theme.textTheme.titleSmall),
        if (done.skipped > 0)
          Text(
            '其中 ${done.skipped} 条此前已经导过，没有重复计费。',
            style: theme.textTheme.bodySmall,
          ),
        if (done.failures > 0)
          Text(
            '有 ${done.failures} 次写入失败。重新导入同一个文件即可 —— '
            '已经写进去的不会重复，只补没成的那些。',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.error,
            ),
          ),
        const SizedBox(height: 8),
        Text(
          '导入在服务端异步进行，可能还要再跑一阵子才会全部出现。',
          style: theme.textTheme.bodySmall?.copyWith(
            color: theme.colorScheme.onSurfaceVariant,
          ),
        ),
      ],
    );
  }
}

/// The daemon's own wording, shown verbatim.
///
/// The parsers are written to fail loudly and print the keys they actually saw
/// — rewriting that into "导入失败" would throw away the only clue about a
/// format change.
class _Problem extends StatelessWidget {
  const _Problem({required this.message});

  final String message;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: scheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: SelectableText(
        message,
        style: Theme.of(
          context,
        ).textTheme.bodySmall?.copyWith(color: scheme.onErrorContainer),
      ),
    );
  }
}
