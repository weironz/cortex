import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/formatting.dart';
import '../../state/memory_controller.dart';
import '../../widgets/empty_state.dart';
import '../../widgets/panel_header.dart';
import 'memory_browser.dart';
import 'widgets/fact_card.dart';

/// Right-hand pane: search the memory graph, inspect provenance, and replay
/// what was known at an earlier instant.
class MemoryPanel extends ConsumerStatefulWidget {
  const MemoryPanel({super.key, this.onClose});

  final VoidCallback? onClose;

  @override
  ConsumerState<MemoryPanel> createState() => _MemoryPanelState();
}

class _MemoryPanelState extends ConsumerState<MemoryPanel> {
  late final TextEditingController _controller = TextEditingController(
    text: ref.read(memoryControllerProvider).query,
  );

  @override
  void initState() {
    super.initState();
    // Show something immediately instead of an empty pane: an empty query
    // returns the most recent facts.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final state = ref.read(memoryControllerProvider);
      if (!state.hasSearched) {
        ref.read(memoryControllerProvider.notifier).search();
      }
    });
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final state = ref.watch(memoryControllerProvider);
    final notifier = ref.read(memoryControllerProvider.notifier);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: '记忆',
          subtitle: state.hasSearched && !state.loading
              ? '${state.result.facts.length} 条结果'
              : null,
          actions: [
            // 整页入口挂在这儿，不再往顶栏加一个图标：想看记忆的人已经
            // 打开了这一栏，而顶栏那一排是应用级的显示开关
            IconButton(
              onPressed: () => MemoryBrowser.open(context),
              iconSize: 17,
              tooltip: '在整页中打开',
              icon: const Icon(Icons.open_in_full_rounded),
            ),
            IconButton(
              onPressed: state.loading ? null : notifier.search,
              iconSize: 17,
              tooltip: '重新检索',
              icon: const Icon(Icons.refresh_rounded),
            ),
            if (widget.onClose != null)
              IconButton(
                onPressed: widget.onClose,
                iconSize: 18,
                tooltip: '收起',
                icon: const Icon(Icons.close_rounded),
              ),
          ],
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 12, 12, 8),
          child: TextField(
            controller: _controller,
            onChanged: notifier.setQuery,
            onSubmitted: (_) => notifier.search(),
            textInputAction: TextInputAction.search,
            decoration: InputDecoration(
              hintText: '检索事实、实体、领域…',
              prefixIcon: Icon(
                Icons.search_rounded,
                size: 18,
                color: scheme.onSurfaceVariant,
              ),
              prefixIconConstraints: const BoxConstraints(
                minWidth: 38,
                minHeight: 38,
              ),
              suffixIcon: state.query.isEmpty
                  ? null
                  : IconButton(
                      iconSize: 15,
                      icon: const Icon(Icons.close_rounded),
                      onPressed: () {
                        _controller.clear();
                        notifier.setQuery('');
                      },
                    ),
            ),
          ),
        ),
        _AsOfBar(state: state, onPick: notifier.setAsOf),
        const Divider(height: 1),
        Expanded(child: _Results(state: state)),
      ],
    );
  }
}

/// Transaction-time replay control.
///
/// Cortex stores both valid time and transaction time; `as_of` filters on the
/// latter, i.e. "hide anything I hadn't learned yet". Exposing it as a first
/// class control is the difference between claiming an audit trail and having
/// one.
class _AsOfBar extends StatelessWidget {
  const _AsOfBar({required this.state, required this.onPick});

  final MemoryState state;
  final ValueChanged<DateTime?> onPick;

  Future<void> _pick(BuildContext context) async {
    final now = DateTime.now();
    final picked = await showDatePicker(
      context: context,
      initialDate: state.asOf ?? now,
      firstDate: now.subtract(const Duration(days: 3650)),
      lastDate: now,
      helpText: '回放到哪一天为止已知的记忆',
    );
    if (picked == null) return;
    // End of the chosen day, so the whole day counts as "already known".
    onPick(DateTime(picked.year, picked.month, picked.day, 23, 59, 59));
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final active = state.isTimeTravelling;

    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 0, 12, 10),
      child: Row(
        children: [
          Expanded(
            child: InkWell(
              onTap: () => _pick(context),
              borderRadius: BorderRadius.circular(8),
              child: Container(
                padding: const EdgeInsets.symmetric(
                  horizontal: 10,
                  vertical: 7,
                ),
                decoration: BoxDecoration(
                  color: active
                      ? scheme.secondary.withValues(alpha: 0.10)
                      : Colors.transparent,
                  borderRadius: BorderRadius.circular(8),
                  border: Border.all(
                    color: active ? scheme.secondary : scheme.outlineVariant,
                  ),
                ),
                child: Row(
                  children: [
                    Icon(
                      active
                          ? Icons.history_toggle_off_rounded
                          : Icons.schedule_rounded,
                      size: 14,
                      color: active
                          ? scheme.secondary
                          : scheme.onSurfaceVariant,
                    ),
                    const SizedBox(width: 7),
                    Expanded(
                      child: Text(
                        active ? '回放至 ${formatDate(state.asOf)}' : '当前时刻的记忆',
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: active
                              ? scheme.secondary
                              : scheme.onSurfaceVariant,
                          fontWeight: active
                              ? FontWeight.w600
                              : FontWeight.w400,
                        ),
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
          if (active) ...[
            const SizedBox(width: 6),
            IconButton(
              onPressed: () => onPick(null),
              iconSize: 15,
              tooltip: '回到现在',
              visualDensity: VisualDensity.compact,
              icon: const Icon(Icons.restore_rounded),
            ),
          ],
        ],
      ),
    );
  }
}

class _Results extends StatelessWidget {
  const _Results({required this.state});

  final MemoryState state;

  @override
  Widget build(BuildContext context) {
    if (state.loading && state.result.isEmpty) {
      return const Center(
        child: SizedBox(
          width: 20,
          height: 20,
          child: CircularProgressIndicator(strokeWidth: 2.2),
        ),
      );
    }

    if (state.error != null) {
      return EmptyState(
        icon: Icons.cloud_off_rounded,
        title: '检索失败',
        description: state.error,
        tone: EmptyStateTone.error,
      );
    }

    if (state.result.isEmpty) {
      // Empty is a legitimate answer, not a failure: the retriever abstains
      // when nothing stored is relevant. Styled as a neutral state on purpose —
      // an error tone here would train the user to distrust a correct result.
      return EmptyState(
        icon: Icons.psychology_outlined,
        title: state.query.isEmpty ? '还没有记忆' : '没有相关的记忆',
        description: state.query.isEmpty
            ? '聊几轮之后，抽取出的事实会出现在这里，'
                  '每条都能追溯到原始对话。'
            : state.isTimeTravelling
            ? '在 ${formatDate(state.asOf)} 这个时点，Cortex 还不知道相关的事。'
            : '检索器判定与已存记忆无关时会主动弃权，返回空是正常结果。'
                  '换个说法可能会命中 —— 检索同时走 BM25 与向量两路。',
      );
    }

    final facts = state.result.facts;
    return ListView.builder(
      padding: const EdgeInsets.fromLTRB(12, 10, 12, 20),
      itemCount: facts.length,
      itemBuilder: (context, i) => FactCard(
        fact: facts[i],
        channels: state.result.channels[facts[i].id],
        invalidated: facts[i].invalidated,
      ),
    );
  }
}
