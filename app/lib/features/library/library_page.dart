/// 资料库 —— agent 随时能取的材料，与某一条对话无关。
///
/// # 它与图片页、与附件的分工
///
/// * 附件属于**那一条消息**：为了问这一句话把图贴上来。
/// * 图片页属于**画出来的东西**：一次生成的产物。
/// * 这里属于**你**：一份 API 文档、一份客户需求、一份规范。不属于任何
///   一轮对话，而是每一轮都可能被取用的背景材料。
///
/// # 为什么最后要说那句「不会自动进每一轮提示词」
///
/// 用户传一份 200 页的规范进来，最合理的预期是「它现在知道这份文件了」。
/// 而实际是模型**想查时才查**（`library_search`）—— 不说清的话，他会
/// 因为模型没有主动引用而以为上传失败了。这句话不是免责声明，是**用法
/// 说明**：知道它按需检索，才会在提问时给出能被检索到的关键词。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/theme.dart';
import '../../models/attachment.dart' show formatBytes;
import '../../models/library_item.dart';
import '../../state/library_controller.dart';
import '../../widgets/empty_state.dart';
import '../../widgets/panel_header.dart';
import '../images/widgets/folder_picker.dart';

class LibraryPageView extends ConsumerWidget {
  const LibraryPageView({super.key, this.onToggleSessions});

  /// 窄屏上左栏是抽屉，这一页里也要有路把它叫出来 —— 少了这个按钮，
  /// 从资料库回会话列表就只剩「从屏幕左缘划」（与图片页同一条理由）。
  final VoidCallback? onToggleSessions;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final state = ref.watch(libraryControllerProvider);
    final n = ref.read(libraryControllerProvider.notifier);

    void said(String message) {
      if (message.isEmpty || !context.mounted) return;
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(message)));
    }

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PanelHeader(
          title: '资料库',
          subtitle: state.unsupported ? null : _subtitle(state),
          leading: onToggleSessions == null
              ? null
              : IconButton(
                  onPressed: onToggleSessions,
                  iconSize: 19,
                  tooltip: '显示会话栏',
                  icon: const Icon(Icons.menu_rounded),
                ),
          actions: [
            if (!state.unsupported)
              // 「按大小」。库满了要问的是「哪几份最占地方」，而按时间排的
              // 列表答不了 —— 一份 300 MB 的日志可能在第八页
              IconButton(
                key: const ValueKey('lib:by-size'),
                onPressed: () => n.setBySize(!state.bySize),
                iconSize: 18,
                isSelected: state.bySize,
                tooltip: state.bySize ? '按时间排' : '按大小排（找出最占地方的）',
                icon: const Icon(Icons.sort_rounded),
              ),
            if (!state.unsupported)
              IconButton(
                key: const ValueKey('lib:refresh'),
                onPressed: n.refresh,
                iconSize: 18,
                tooltip: '刷新',
                icon: const Icon(Icons.refresh_rounded),
              ),
          ],
        ),
        if (!state.unsupported) _TabBar(state: state, onPick: n.setTab),
        Expanded(child: _body(context, ref, state, said)),
        if (!state.unsupported && state.items.isNotEmpty) const _Footnote(),
      ],
    );
  }

  /// 副标题：几项、占了多少。
  ///
  /// # 为什么占用要摆在这儿
  ///
  /// 附件与交付物是**自动**收进来的（判据见 `docs/library-content.md`：
  /// 不判断值不值得收）。那个决定成立的前提是膨胀有办法管 —— 而办法就是
  /// 「看得见、删得掉」。不画出来的话，用户第一次知道有上限是在它满了、
  /// 东西静默收不进去的那一刻。
  ///
  /// 没有上限（本机开发把 `CORTEX_LIBRARY_QUOTA_BYTES` 设成 0）时只报占用，
  /// 不报「/ 0」—— 一个分母是 0 的比例只会让人以为坏了。
  String _subtitle(LibraryState state) {
    final u = state.usage;
    final used = formatBytes(u.bytes);
    final size = u.hasQuota ? '$used / ${formatBytes(u.quotaBytes)}' : used;
    return '${state.items.length} 项 · $size · agent 随时能取的材料，'
        '与某一条对话无关';
  }

  Widget _body(
    BuildContext context,
    WidgetRef ref,
    LibraryState state,
    void Function(String) said,
  ) {
    if (state.unsupported) {
      // **不给重试按钮**：这不是故障，重试一百次也不会有
      return const EmptyState(
        icon: Icons.library_books_outlined,
        title: '这个部署没有资料库',
        description: '资料库要一个接了数据库的服务端 —— 离线模式与纯本机部署没有这条路。',
      );
    }
    if (state.loading && state.items.isEmpty) {
      return const Center(
        child: SizedBox(
          width: 18,
          height: 18,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
      );
    }
    if (state.error != null) {
      return EmptyState(
        icon: Icons.cloud_off_rounded,
        title: '拉不到资料库',
        description: state.error,
        tone: EmptyStateTone.error,
        action: OutlinedButton.icon(
          onPressed: ref.read(libraryControllerProvider.notifier).refresh,
          icon: const Icon(Icons.refresh_rounded, size: 16),
          label: const Text('重试'),
        ),
      );
    }
    if (state.items.isEmpty && state.folders.isEmpty) {
      return const EmptyState(
        icon: Icons.library_books_outlined,
        title: '资料库还是空的',
        // 说清**怎么放进来**：一个没有入口说明的空态等于死路。
        //
        // ⚠️ 这句话改过两次，两次都是因为它在答应做不到的事：
        //
        // 1. 原文「把文件拖进对话里发出去，它就会进资料库」—— 附件那条路
        //    一行都没碰过 library，生产上 `library_items` 一直是 0 行
        // 2. 第二版改成「右键存进资料库」—— 做到了，但用户当场指出
        //    「不是多此一举吗，ChatGPT 和 Gemini 都是自动的」。他是对的
        //
        // 现在是服务端在 `POST /episodes` 上自动收（判据见
        // `docs/library-content.md`：不判断值不值得收，漏收补救不了，
        // 膨胀补救得了）。所以这一屏空着只有一个意思：还没发过带附件的消息。
        description:
            '在对话里发出去的文件和图片会自动收进这里。\n'
            '之后模型在需要时会自己去检索它们。',
      );
    }
    return _Grid(state: state, said: said);
  }
}

/// 三个页签：全部 / 图片 / 文件。
class _TabBar extends ConsumerWidget {
  const _TabBar({required this.state, required this.onPick});

  final LibraryState state;
  final Future<void> Function(LibraryTab) onPick;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final tokens = theme.cortex;
    return Padding(
      padding: const EdgeInsets.fromLTRB(14, 8, 14, 4),
      child: Row(
        children: [
          for (final t in LibraryTab.values) ...[
            Material(
              color: state.tab == t ? tokens.sidebarAccent : Colors.transparent,
              borderRadius: BorderRadius.circular(CortexTokens.radiusRow),
              child: InkWell(
                key: ValueKey('lib:tab:${t.name}'),
                onTap: () => onPick(t),
                borderRadius: BorderRadius.circular(CortexTokens.radiusRow),
                child: Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 12,
                    vertical: 6,
                  ),
                  child: Text(
                    t.label,
                    style: theme.textTheme.labelMedium?.copyWith(
                      // 选中态补一档字重、不用更亮的颜色 —— 与右栏页签、
                      // 侧栏选中行同一套写法
                      fontWeight: state.tab == t ? FontWeight.w600 : null,
                      color: state.tab == t
                          ? theme.colorScheme.onSurface
                          : tokens.foregroundTertiary,
                    ),
                  ),
                ),
              ),
            ),
            const SizedBox(width: 4),
          ],
          const Spacer(),
          // 进了某个文件夹之后要有路出来 —— 少了它，用户只能靠左栏
          // 重新点一次「资料库」，而那会连页签一起重置
          if (state.folder != null)
            TextButton.icon(
              key: const ValueKey('lib:allfolders'),
              onPressed: () =>
                  ref.read(libraryControllerProvider.notifier).setFolder(null),
              icon: const Icon(Icons.arrow_back_rounded, size: 15),
              label: const Text('全部'),
            ),
        ],
      ),
    );
  }
}

/// 文件夹一段 + 未归档一段。
class _Grid extends ConsumerWidget {
  const _Grid({required this.state, required this.said});

  final LibraryState state;
  final void Function(String) said;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final n = ref.read(libraryControllerProvider.notifier);
    final unfiled = state.folder == null ? state.unfiled : state.items;

    return ListView(
      padding: const EdgeInsets.fromLTRB(14, 6, 14, 14),
      children: [
        if (state.selected.isNotEmpty) _SelectionBar(state: state, said: said),
        if (state.folder == null && state.folders.isNotEmpty) ...[
          _SectionTitle(title: '文件夹', hint: '图片和文件都能放进来 —— 一份东西只在一个文件夹里'),
          Wrap(
            spacing: 10,
            runSpacing: 10,
            children: [
              for (final f in state.folders)
                _FolderCard(
                  name: f.name,
                  count: f.count,
                  onTap: () => n.setFolder(f.id),
                ),
            ],
          ),
          const SizedBox(height: 18),
        ],
        _SectionTitle(
          title: state.folder == null ? '未归档' : '这个文件夹里',
          hint: state.selected.isEmpty ? '选中后可以「移动到文件夹」' : null,
        ),
        if (unfiled.isEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 18),
            child: Text(
              state.folder == null ? '都归好档了。' : '这个文件夹还是空的。',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
          )
        else
          Wrap(
            spacing: 10,
            runSpacing: 10,
            children: [
              for (final item in unfiled)
                _ItemCard(
                  item: item,
                  selected: state.selected.contains(item.id),
                  onTap: () => n.toggleSelect(item.id),
                ),
            ],
          ),
      ],
    );
  }
}

class _SectionTitle extends StatelessWidget {
  const _SectionTitle({required this.title, this.hint});

  final String title;
  final String? hint;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.baseline,
        textBaseline: TextBaseline.alphabetic,
        children: [
          Text(title, style: theme.textTheme.titleSmall),
          if (hint != null) ...[
            const SizedBox(width: 9),
            Flexible(
              child: Text(
                hint!,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                ),
              ),
            ),
          ],
        ],
      ),
    );
  }
}

class _FolderCard extends StatelessWidget {
  const _FolderCard({
    required this.name,
    required this.count,
    required this.onTap,
  });

  final String name;
  final int count;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return SizedBox(
      width: 196,
      child: Material(
        color: scheme.surfaceContainer,
        borderRadius: BorderRadius.circular(CortexTokens.radiusCard),
        child: InkWell(
          onTap: onTap,
          borderRadius: BorderRadius.circular(CortexTokens.radiusCard),
          child: Container(
            height: 92,
            padding: const EdgeInsets.all(14),
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(CortexTokens.radiusCard),
              border: Border.all(color: scheme.outlineVariant),
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Icon(
                      Icons.folder_rounded,
                      size: 17,
                      color: theme.cortex.accentInk,
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text(
                        name,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodyMedium?.copyWith(
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                    ),
                  ],
                ),
                const Spacer(),
                Text(
                  '$count 项',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: theme.cortex.foregroundTertiary,
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _ItemCard extends StatelessWidget {
  const _ItemCard({
    required this.item,
    required this.selected,
    required this.onTap,
  });

  final LibraryItem item;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return SizedBox(
      width: 196,
      child: Material(
        color: scheme.surfaceContainer,
        borderRadius: BorderRadius.circular(CortexTokens.radiusCard),
        child: InkWell(
          key: ValueKey('lib:item:${item.id}'),
          onTap: onTap,
          borderRadius: BorderRadius.circular(CortexTokens.radiusCard),
          child: Container(
            padding: const EdgeInsets.all(12),
            decoration: BoxDecoration(
              borderRadius: BorderRadius.circular(CortexTokens.radiusCard),
              border: Border.all(
                // 选中用**边框**而不是换底色：卡片本来就有底，
                // 换底会让选中态与 hover 撞在一起
                color: selected ? scheme.primary : scheme.outlineVariant,
                width: selected ? 1.5 : 1,
              ),
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Icon(
                      item.isImage
                          ? Icons.image_outlined
                          : Icons.description_outlined,
                      size: 16,
                      color: scheme.onSurfaceVariant,
                    ),
                    const Spacer(),
                    _OriginBadge(origin: item.origin),
                  ],
                ),
                const SizedBox(height: 10),
                Text(
                  item.name,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodySmall?.copyWith(
                    fontWeight: FontWeight.w600,
                  ),
                ),
                const SizedBox(height: 4),
                Text(
                  item.subtitle,
                  style: theme.textTheme.labelSmall?.copyWith(
                    // 提不出正文的那句用琥珀 —— 它是「你要知道的事」
                    // （这份材料模型检索不到），不是普通的元信息
                    color: item.chunkState == ChunkState.unsupported
                        ? theme.cortex.warning
                        : theme.cortex.foregroundTertiary,
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _OriginBadge extends StatelessWidget {
  const _OriginBadge({required this.origin});

  final String origin;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final generated = origin == 'generated';
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(
          generated ? Icons.auto_awesome_outlined : Icons.upload_outlined,
          size: 11,
          color: theme.cortex.foregroundTertiary,
        ),
        const SizedBox(width: 4),
        Text(
          generated ? '已生成' : '已上传',
          style: theme.textTheme.labelSmall?.copyWith(
            color: theme.cortex.foregroundTertiary,
          ),
        ),
      ],
    );
  }
}

class _SelectionBar extends ConsumerWidget {
  const _SelectionBar({required this.state, required this.said});

  final LibraryState state;
  final void Function(String) said;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final n = ref.read(libraryControllerProvider.notifier);
    return Padding(
      padding: const EdgeInsets.only(bottom: 12),
      child: Row(
        children: [
          Text(
            '选中 ${state.selected.length} 项',
            style: theme.textTheme.titleSmall,
          ),
          const SizedBox(width: 8),
          TextButton(onPressed: n.clearSelection, child: const Text('取消')),
          const Spacer(),
          TextButton.icon(
            key: const ValueKey('lib:sel:move'),
            onPressed: () async {
              final folder = await pickFolder(context, ref, said: said);
              if (folder == null) return;
              said(await n.moveSelectedTo(folder.id, folder.name));
            },
            icon: const Icon(Icons.drive_file_move_outlined, size: 16),
            label: const Text('移动到文件夹'),
          ),
          if (state.folder != null) ...[
            const SizedBox(width: 4),
            TextButton.icon(
              key: const ValueKey('lib:sel:unfile'),
              onPressed: () async => said(await n.moveSelectedTo(null, '')),
              icon: const Icon(Icons.folder_off_outlined, size: 16),
              label: const Text('移出文件夹'),
            ),
          ],
          const SizedBox(width: 4),
          TextButton.icon(
            key: const ValueKey('lib:sel:remove'),
            onPressed: () async => said(await n.removeSelected()),
            icon: const Icon(Icons.delete_outline_rounded, size: 16),
            // **叫「从资料库移除」不叫「删除」** —— blob 不动，
            // 对话里引用过的照常显示（与图库那条同一纪律）
            label: const Text('从资料库移除'),
          ),
        ],
      ),
    );
  }
}

/// 底下那句用法说明。见文件头「为什么最后要说那句」。
class _Footnote extends StatelessWidget {
  const _Footnote();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 0, 16, 12),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(
            Icons.info_outline_rounded,
            size: 14,
            color: theme.cortex.foregroundTertiary,
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Text.rich(
              TextSpan(
                style: theme.textTheme.labelSmall?.copyWith(
                  color: theme.cortex.foregroundTertiary,
                  height: 1.6,
                ),
                children: [
                  const TextSpan(text: '资料库里的东西'),
                  TextSpan(
                    text: '不会',
                    style: TextStyle(
                      fontWeight: FontWeight.w700,
                      color: theme.colorScheme.onSurface,
                    ),
                  ),
                  const TextSpan(
                    text:
                        '自动进每一轮提示词 —— 模型按关键词检索。'
                        '哪一份被用过，会写在那一轮的工具调用里。',
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}
