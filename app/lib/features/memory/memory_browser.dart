import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/formatting.dart';
import '../../state/memory_controller.dart';
import '../../widgets/empty_state.dart';
import 'widgets/fact_card.dart';

/// 整页的记忆浏览器。
///
/// # 为什么右栏那块不够
///
/// 该有的东西其实都在那儿了：检索、`source_channel` / `trust_tier` 的配色、
/// 点开看源 episode 的 provenance、按系统时间回放。**但它是一条 320px 的
/// 边栏**，而且只在对话页里够得着。
///
/// 这三样恰好是这个项目唯一不可替代的部分（别家给的是一个相似度分数）。
/// 挤在边栏里等于把它们藏起来 —— 「能看见自己的记忆」本身就该是一件事，
/// 不是聊天的附属品。
///
/// # 与右栏共用同一个 controller
///
/// 不新开一份状态：两处显示的必须是同一次检索的结果。各持一份的话，
/// 「边栏里有这条、整页里没有」会变成一个没人解释得清的现象，
/// 而它只在两边同时打开时才出现。
class MemoryBrowser extends ConsumerStatefulWidget {
  const MemoryBrowser({super.key});

  static Future<void> open(BuildContext context) => Navigator.of(
    context,
  ).push(MaterialPageRoute<void>(builder: (_) => const MemoryBrowser()));

  @override
  ConsumerState<MemoryBrowser> createState() => _MemoryBrowserState();
}

class _MemoryBrowserState extends ConsumerState<MemoryBrowser> {
  late final TextEditingController _query = TextEditingController(
    text: ref.read(memoryControllerProvider).query,
  );

  /// 时间轴的左端。**只往早走，不往晚回。**
  ///
  /// 它取自当前结果里最早那条的 `created_at`。而结果会随着拖动而变 ——
  /// 如果跟着一起缩，滑块会在拖的过程中自己改变量程，手感是「越拖越跑」。
  /// 单调放宽让量程稳下来，代价只是回到现在之后左端仍停在那个更早的日期，
  /// 而那恰好是对的：那条记忆确实存在过。
  DateTime? _earliest;

  /// 拖动过程中的位置。松手才真的去检索 —— 每帧发一次请求既打后端，
  /// 也让结果在手指底下不停跳。
  double? _dragging;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!ref.read(memoryControllerProvider).hasSearched) {
        ref.read(memoryControllerProvider.notifier).search();
      }
    });
  }

  @override
  void dispose() {
    _query.dispose();
    super.dispose();
  }

  /// 从结果里把时间轴左端放宽。
  void _widen(MemoryState state) {
    final stamps = state.result.facts
        .map((f) => f.createdAt)
        .whereType<DateTime>();
    if (stamps.isEmpty) return;
    final oldest = stamps.reduce((a, b) => a.isBefore(b) ? a : b);
    if (_earliest == null || oldest.isBefore(_earliest!)) {
      // build 里改状态要排到帧后，否则是「build 期间 setState」
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) setState(() => _earliest = oldest);
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final state = ref.watch(memoryControllerProvider);
    final notifier = ref.read(memoryControllerProvider.notifier);
    _widen(state);

    return Scaffold(
      appBar: AppBar(
        title: const Text('记忆'),
        // 检索到几条是这一页最要紧的一个数字：它同时回答「有没有」与
        // 「回放到那一刻少了多少」
        actions: [
          if (state.hasSearched && !state.loading)
            Padding(
              padding: const EdgeInsets.only(right: 16),
              child: Center(
                child: Text(
                  '${state.result.facts.length} 条',
                  style: theme.textTheme.labelMedium?.copyWith(
                    color: scheme.onSurfaceVariant,
                  ),
                ),
              ),
            ),
        ],
      ),
      body: Column(
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(20, 14, 20, 8),
            child: Center(
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 900),
                child: TextField(
                  controller: _query,
                  autofocus: true,
                  onChanged: notifier.setQuery,
                  onSubmitted: (_) => notifier.search(),
                  textInputAction: TextInputAction.search,
                  decoration: InputDecoration(
                    hintText: '检索事实、实体、领域…　留空则是此刻的全貌',
                    prefixIcon: const Icon(Icons.search_rounded, size: 20),
                    suffixIcon: state.query.isEmpty
                        ? null
                        : IconButton(
                            icon: const Icon(Icons.close_rounded, size: 17),
                            onPressed: () {
                              _query.clear();
                              notifier.setQuery('');
                              notifier.search();
                            },
                          ),
                  ),
                ),
              ),
            ),
          ),
          _Timeline(
            earliest: _earliest,
            asOf: state.asOf,
            dragging: _dragging,
            onDrag: (v) => setState(() => _dragging = v),
            onSettled: (at) {
              setState(() => _dragging = null);
              notifier.setAsOf(at);
              notifier.search();
            },
          ),
          const Divider(height: 1),
          Expanded(child: _Grid(state: state)),
        ],
      ),
    );
  }
}

/// 能拖的时间轴 —— 这一页唯一的新东西。
///
/// # 为什么日期选择器不够
///
/// 双时间轴回放是这个项目最难被复制的一样，而它的说服力全在**连续性**上：
/// 拖着滑块往回走，看着一条条事实消失、剩下的那些就是你当时真正知道的。
/// 选个日期再点确定，同一件事变成了一次查询 —— 结果一样，但没人会「看见」它。
///
/// # 松手才检索
///
/// 每帧发一次请求既打后端，也让结果在手指底下不停跳。拖的时候只改上面
/// 那行日期，松手才真的去查 —— 那一下的「刷新」正好是想要的那个瞬间。
class _Timeline extends StatelessWidget {
  const _Timeline({
    required this.earliest,
    required this.asOf,
    required this.dragging,
    required this.onDrag,
    required this.onSettled,
  });

  final DateTime? earliest;
  final DateTime? asOf;
  final double? dragging;
  final ValueChanged<double> onDrag;
  final ValueChanged<DateTime?> onSettled;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final now = DateTime.now();
    final start = earliest;

    // 还没有结果、或者所有记忆都在同一刻 —— 没有可拖的区间。
    // 画一条禁用的轨道而不是整个不画：位置固定，用户不会觉得它「有时候才有」
    final span = start == null ? Duration.zero : now.difference(start);
    final enabled = span.inMinutes > 0;

    final position =
        dragging ??
        (asOf == null || !enabled
            ? 1.0
            : (asOf!.difference(start!).inMinutes / span.inMinutes).clamp(
                0.0,
                1.0,
              ));
    final at = !enabled || position >= 1.0
        ? null
        : start!.add(Duration(minutes: (span.inMinutes * position).round()));

    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 900),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 0, 20, 6),
          child: Row(
            children: [
              Icon(
                at == null
                    ? Icons.schedule_rounded
                    : Icons.history_toggle_off_rounded,
                size: 16,
                color: at == null ? scheme.onSurfaceVariant : scheme.secondary,
              ),
              const SizedBox(width: 8),
              SizedBox(
                width: 168,
                child: Text(
                  at == null ? '当前时刻的记忆' : '回放至 ${formatDate(at)}',
                  style: theme.textTheme.labelMedium?.copyWith(
                    color: at == null
                        ? scheme.onSurfaceVariant
                        : scheme.secondary,
                    fontWeight: at == null ? null : FontWeight.w600,
                  ),
                ),
              ),
              Expanded(
                child: Slider(
                  value: position,
                  onChanged: enabled ? onDrag : null,
                  onChangeEnd: enabled
                      ? (v) => onSettled(
                          v >= 1.0
                              ? null
                              : start!.add(
                                  Duration(
                                    minutes: (span.inMinutes * v).round(),
                                  ),
                                ),
                        )
                      : null,
                ),
              ),
              if (at != null)
                TextButton(
                  onPressed: () => onSettled(null),
                  child: const Text('回到现在'),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 宽屏下多列铺开。侧栏里是一列，那是宽度决定的，不是设计决定的。
class _Grid extends StatelessWidget {
  const _Grid({required this.state});

  final MemoryState state;

  @override
  Widget build(BuildContext context) {
    if (state.loading && state.result.isEmpty) {
      return const Center(
        child: SizedBox(
          width: 22,
          height: 22,
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
      return EmptyState(
        icon: Icons.psychology_outlined,
        title: state.query.isEmpty ? '还没有记忆' : '没有相关的记忆',
        description: state.isTimeTravelling
            ? '在 ${formatDate(state.asOf)} 这个时点，Cortex 还不知道相关的事。'
            : state.query.isEmpty
            ? '聊几轮之后，抽取出的事实会出现在这里，每条都能追溯到原始对话。'
            : '检索器判定与已存记忆无关时会主动弃权，返回空是正常结果。',
      );
    }

    final facts = state.result.facts;
    return LayoutBuilder(
      builder: (context, box) {
        // 一列 420px 上下。再宽就让列变宽而不是加列 —— 一条事实是一句话，
        // 排成太窄的一条会被折成四五行
        final columns = (box.maxWidth / 420).floor().clamp(1, 3);
        return Center(
          child: ConstrainedBox(
            constraints: BoxConstraints(maxWidth: columns * 460),
            child: GridView.builder(
              padding: const EdgeInsets.fromLTRB(20, 14, 20, 28),
              gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
                crossAxisCount: columns,
                crossAxisSpacing: 12,
                mainAxisSpacing: 12,
                // 卡片高度不定（陈述长短不一），给一个偏矮的比例让它自己撑。
                // mainAxisExtent 定死会把长句子截掉
                mainAxisExtent: 152,
              ),
              itemCount: facts.length,
              itemBuilder: (context, i) => SingleChildScrollView(
                child: FactCard(
                  fact: facts[i],
                  channels: state.result.channels[facts[i].id],
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}
