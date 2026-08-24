/// 图库上面那一条：看哪个文件夹，以及勾中之后能做什么。
///
/// # 为什么文件夹是「过滤条」，不是一个单独的页面
///
/// immich 的文件夹是一个能点进去的东西（有封面、有标题页）。那一套要一条
/// 自己的导航路径，而这一页本身已经被「落地页 ↔ 对话」占了一个形态开关 ——
/// 再叠一层「图库 ↔ 文件夹详情」，用户按返回时根本猜不到会回到哪儿。
///
/// 一条过滤器少了封面墙，但**换文件夹不改变周围任何东西**：还在这一页、
/// 输入框还在上面、右键菜单还是那一份。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../models/generated_image.dart';
import '../../../state/app_providers.dart';
import '../../../state/image_controller.dart';
import 'folder_picker.dart';

class FolderBar extends ConsumerWidget {
  const FolderBar({super.key, required this.said});

  /// 做完一件事之后要说的那句话（页面拿它弹 SnackBar）。
  final void Function(String message) said;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final folders =
        ref.watch(foldersProvider).value?.folders ?? const <Folder>[];
    final current = ref.watch(imageControllerProvider.select((s) => s.folder));
    final selected = ref.watch(
      imageControllerProvider.select((s) => s.selected),
    );

    // 勾中了东西时，这一条整个换成「对这几张做什么」——
    // 两排按钮叠着的话，用户得先分清哪一排管的是哪件事
    if (selected.isNotEmpty) {
      return _SelectionBar(
        count: selected.length,
        folders: folders,
        said: said,
      );
    }

    return Row(
      children: [
        Text('我的图片', style: theme.textTheme.titleSmall),
        const SizedBox(width: 12),
        Expanded(
          child: SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            child: Row(
              children: [
                _Chip(
                  key: const ValueKey('folder:all'),
                  label: '全部',
                  selected: current == null,
                  onTap: () => ref
                      .read(imageControllerProvider.notifier)
                      .setFolder(null),
                ),
                for (final a in folders) ...[
                  const SizedBox(width: 6),
                  _Chip(
                    key: ValueKey('folder:${a.id}'),
                    label: '${a.name} · ${a.count}',
                    selected: current == a.id,
                    onTap: () => ref
                        .read(imageControllerProvider.notifier)
                        .setFolder(a.id),
                    onLongPress: () => _manage(context, ref, a),
                    onSecondary: () => _manage(context, ref, a),
                  ),
                ],
              ],
            ),
          ),
        ),
        const SizedBox(width: 8),
        TextButton.icon(
          key: const ValueKey('folder:new'),
          onPressed: () => _create(context, ref),
          icon: const Icon(Icons.create_new_folder_outlined, size: 15),
          label: const Text('新建文件夹'),
        ),
      ],
    );
  }

  Future<void> _create(BuildContext context, WidgetRef ref) async {
    final name = await askFolderName(context, title: '新建文件夹');
    if (name == null) return;
    try {
      await ref.read(cortexApiProvider).createFolder(name);
      ref.invalidate(foldersProvider);
      said('文件夹「$name」建好了。');
    } on Object catch (e) {
      said('$e');
    }
  }

  Future<void> _manage(BuildContext context, WidgetRef ref, Folder a) async {
    final action = await showModalBottomSheet<String>(
      context: context,
      builder: (ctx) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              leading: const Icon(Icons.drive_file_rename_outline, size: 20),
              title: const Text('重命名'),
              onTap: () => Navigator.of(ctx).pop('rename'),
            ),
            ListTile(
              leading: const Icon(Icons.folder_delete_outlined, size: 20),
              title: const Text('删除这个文件夹'),
              // ⚠️ **必须说清图不会没。** 不说的话，一个「删除文件夹」按钮
              // 读起来就是「连里面的图一起删」，而没人敢按
              subtitle: const Text('里面的图一张都不会少，只是不再属于这个文件夹'),
              onTap: () => Navigator.of(ctx).pop('delete'),
            ),
          ],
        ),
      ),
    );
    if (action == null || !context.mounted) return;

    if (action == 'rename') {
      final name = await askFolderName(context, title: '重命名', initial: a.name);
      if (name == null) return;
      try {
        await ref.read(cortexApiProvider).renameFolder(a.id, name);
        ref.invalidate(foldersProvider);
      } on Object catch (e) {
        said('$e');
      }
      return;
    }

    try {
      await ref.read(cortexApiProvider).deleteFolder(a.id);
      ref.invalidate(foldersProvider);
      // 正在看的就是它 —— 退回「全部」，否则界面会停在一个不存在的文件夹上
      if (ref.read(imageControllerProvider).folder == a.id) {
        await ref.read(imageControllerProvider.notifier).setFolder(null);
      }
      said('文件夹「${a.name}」删了，里面的图都还在。');
    } on Object catch (e) {
      said('$e');
    }
  }
}

/// 勾中之后那一条。
class _SelectionBar extends ConsumerWidget {
  const _SelectionBar({
    required this.count,
    required this.folders,
    required this.said,
  });

  final int count;
  final List<Folder> folders;
  final void Function(String message) said;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final current = ref.watch(imageControllerProvider.select((s) => s.folder));
    final n = ref.read(imageControllerProvider.notifier);

    return Row(
      children: [
        IconButton(
          key: const ValueKey('sel:clear'),
          tooltip: '取消选择',
          iconSize: 18,
          onPressed: n.clearSelection,
          icon: const Icon(Icons.close_rounded),
        ),
        Text(
          '选中 $count 张',
          key: const ValueKey('sel:count'),
          style: theme.textTheme.titleSmall,
        ),
        const SizedBox(width: 8),
        TextButton(
          key: const ValueKey('sel:all'),
          onPressed: n.selectAll,
          child: const Text('全选'),
        ),
        const Spacer(),
        TextButton.icon(
          key: const ValueKey('sel:add'),
          onPressed: () => _addTo(context, ref),
          icon: const Icon(Icons.playlist_add_rounded, size: 16),
          label: const Text('加入文件夹'),
        ),
        // 「从文件夹里拿出去」只在**看着某个文件夹**时有意义 ——
        // 在「全部」里它答不上来「从哪个里拿」
        if (current != null) ...[
          const SizedBox(width: 4),
          TextButton.icon(
            key: const ValueKey('sel:pull'),
            onPressed: () async {
              final a = folders.where((a) => a.id == current).firstOrNull;
              if (a == null) return;
              said(await n.moveSelectedTo(null, a.name));
            },
            icon: const Icon(Icons.playlist_remove_rounded, size: 16),
            label: const Text('移出文件夹'),
          ),
        ],
        const SizedBox(width: 4),
        TextButton.icon(
          key: const ValueKey('sel:remove'),
          onPressed: () async {
            final ok = await _confirm(context, count);
            if (!ok) return;
            said(await n.removeSelected());
          },
          icon: Icon(
            Icons.delete_outline_rounded,
            size: 16,
            color: theme.colorScheme.error,
          ),
          label: Text(
            '从图库移除',
            style: TextStyle(color: theme.colorScheme.error),
          ),
        ),
      ],
    );
  }

  Future<void> _addTo(BuildContext context, WidgetRef ref) async {
    // 选择器与单张右键菜单**共用一份**，见 `folder_picker.dart` 的头注释
    final folder = await pickFolder(
      context,
      ref,
      said: said,
      title: '移动到哪个文件夹',
    );
    if (folder == null) return;
    final n = ref.read(imageControllerProvider.notifier);
    said(await n.moveSelectedTo(folder.id, folder.name));
  }
}

Future<bool> _confirm(BuildContext context, int count) async =>
    await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text('从图库移除 $count 张？'),
        // **叫「从图库移除」，不叫「删除」** —— 对话里那些照常显示，
        // 说成删除的话用户会以为历史也被改了
        content: const Text('对话里那些照常显示，只是不再出现在这面墙上。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(false),
            child: const Text('算了'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(ctx).pop(true),
            child: const Text('移除'),
          ),
        ],
      ),
    ) ??
    false;

class _Chip extends StatelessWidget {
  const _Chip({
    super.key,
    required this.label,
    required this.selected,
    required this.onTap,
    this.onLongPress,
    this.onSecondary,
  });

  final String label;
  final bool selected;
  final VoidCallback onTap;
  final VoidCallback? onLongPress;
  final VoidCallback? onSecondary;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return GestureDetector(
      onSecondaryTap: onSecondary,
      child: Material(
        color: selected
            ? scheme.secondaryContainer
            : theme.cortex.sidebarAccent,
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        child: InkWell(
          onTap: onTap,
          onLongPress: onLongPress,
          borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 5),
            child: Text(
              label,
              style: theme.textTheme.labelSmall?.copyWith(
                // 未选中的用二级前景（`onSurfaceVariant`），不用第三级：
                // 这一排是**可点的过滤器**，画成第三级会读成禁用
                color: selected
                    ? scheme.onSecondaryContainer
                    : scheme.onSurfaceVariant,
              ),
            ),
          ),
        ),
      ),
    );
  }
}
