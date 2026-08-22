/// 图库上面那一条：看哪个相册，以及勾中之后能做什么。
///
/// # 为什么相册是「过滤条」，不是一个单独的页面
///
/// immich 的相册是一个能点进去的东西（有封面、有标题页）。那一套要一条
/// 自己的导航路径，而这一页本身已经被「落地页 ↔ 对话」占了一个形态开关 ——
/// 再叠一层「图库 ↔ 相册详情」，用户按返回时根本猜不到会回到哪儿。
///
/// 一条过滤器少了封面墙，但**换相册不改变周围任何东西**：还在这一页、
/// 输入框还在上面、右键菜单还是那一份。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../models/generated_image.dart';
import '../../../state/app_providers.dart';
import '../../../state/image_controller.dart';

class AlbumBar extends ConsumerWidget {
  const AlbumBar({super.key, required this.said});

  /// 做完一件事之后要说的那句话（页面拿它弹 SnackBar）。
  final void Function(String message) said;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final albums = ref.watch(albumsProvider).value?.albums ?? const <Album>[];
    final current = ref.watch(imageControllerProvider.select((s) => s.album));
    final selected = ref.watch(
      imageControllerProvider.select((s) => s.selected),
    );

    // 勾中了东西时，这一条整个换成「对这几张做什么」——
    // 两排按钮叠着的话，用户得先分清哪一排管的是哪件事
    if (selected.isNotEmpty) {
      return _SelectionBar(count: selected.length, albums: albums, said: said);
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
                  key: const ValueKey('album:all'),
                  label: '全部',
                  selected: current == null,
                  onTap: () =>
                      ref.read(imageControllerProvider.notifier).setAlbum(null),
                ),
                for (final a in albums) ...[
                  const SizedBox(width: 6),
                  _Chip(
                    key: ValueKey('album:${a.id}'),
                    label: '${a.name} · ${a.count}',
                    selected: current == a.id,
                    onTap: () => ref
                        .read(imageControllerProvider.notifier)
                        .setAlbum(a.id),
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
          key: const ValueKey('album:new'),
          onPressed: () => _create(context, ref),
          icon: const Icon(Icons.create_new_folder_outlined, size: 15),
          label: const Text('新建相册'),
        ),
      ],
    );
  }

  Future<void> _create(BuildContext context, WidgetRef ref) async {
    final name = await _askName(context, title: '新建相册');
    if (name == null) return;
    try {
      await ref.read(cortexApiProvider).createAlbum(name);
      ref.invalidate(albumsProvider);
      said('相册「$name」建好了。');
    } on Object catch (e) {
      said('$e');
    }
  }

  Future<void> _manage(BuildContext context, WidgetRef ref, Album a) async {
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
              title: const Text('删除这个相册'),
              // ⚠️ **必须说清图不会没。** 不说的话，一个「删除相册」按钮
              // 读起来就是「连里面的图一起删」，而没人敢按
              subtitle: const Text('里面的图一张都不会少，只是不再属于这个相册'),
              onTap: () => Navigator.of(ctx).pop('delete'),
            ),
          ],
        ),
      ),
    );
    if (action == null || !context.mounted) return;

    if (action == 'rename') {
      final name = await _askName(context, title: '重命名', initial: a.name);
      if (name == null) return;
      try {
        await ref.read(cortexApiProvider).renameAlbum(a.id, name);
        ref.invalidate(albumsProvider);
      } on Object catch (e) {
        said('$e');
      }
      return;
    }

    try {
      await ref.read(cortexApiProvider).deleteAlbum(a.id);
      ref.invalidate(albumsProvider);
      // 正在看的就是它 —— 退回「全部」，否则界面会停在一个不存在的相册上
      if (ref.read(imageControllerProvider).album == a.id) {
        await ref.read(imageControllerProvider.notifier).setAlbum(null);
      }
      said('相册「${a.name}」删了，里面的图都还在。');
    } on Object catch (e) {
      said('$e');
    }
  }
}

/// 勾中之后那一条。
class _SelectionBar extends ConsumerWidget {
  const _SelectionBar({
    required this.count,
    required this.albums,
    required this.said,
  });

  final int count;
  final List<Album> albums;
  final void Function(String message) said;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final current = ref.watch(imageControllerProvider.select((s) => s.album));
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
          label: const Text('加入相册'),
        ),
        // 「从相册里拿出去」只在**看着某个相册**时有意义 ——
        // 在「全部」里它答不上来「从哪个里拿」
        if (current != null) ...[
          const SizedBox(width: 4),
          TextButton.icon(
            key: const ValueKey('sel:pull'),
            onPressed: () async {
              final a = albums.where((a) => a.id == current).firstOrNull;
              if (a == null) return;
              said(await n.setAlbumMembership(a.id, a.name, add: false));
            },
            icon: const Icon(Icons.playlist_remove_rounded, size: 16),
            label: const Text('移出相册'),
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
    final n = ref.read(imageControllerProvider.notifier);
    final picked = await showModalBottomSheet<Album>(
      context: context,
      builder: (ctx) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Padding(
              padding: EdgeInsets.fromLTRB(16, 14, 16, 6),
              child: Align(
                alignment: Alignment.centerLeft,
                child: Text('加进哪个相册'),
              ),
            ),
            if (albums.isEmpty)
              const Padding(
                padding: EdgeInsets.fromLTRB(16, 4, 16, 12),
                child: Align(
                  alignment: Alignment.centerLeft,
                  // 一个空列表什么都不说的话，用户会以为这个功能坏了
                  child: Text('还没有相册 —— 下面新建一个。'),
                ),
              ),
            for (final a in albums)
              ListTile(
                key: ValueKey('addto:${a.id}'),
                leading: const Icon(Icons.photo_album_outlined, size: 20),
                title: Text(a.name),
                subtitle: Text('${a.count} 张'),
                onTap: () => Navigator.of(ctx).pop(a),
              ),
            ListTile(
              key: const ValueKey('addto:new'),
              leading: const Icon(Icons.create_new_folder_outlined, size: 20),
              title: const Text('新建相册…'),
              onTap: () => Navigator.of(ctx).pop(),
            ),
          ],
        ),
      ),
    );
    if (!context.mounted) return;

    var album = picked;
    if (album == null) {
      // 上面那个 sheet 关掉时也是 null（点了外面）。用一个显式的取消判据：
      // 只有点「新建相册…」才继续问名字
      final name = await _askName(context, title: '新建相册');
      if (name == null) return;
      try {
        final all = await ref.read(cortexApiProvider).createAlbum(name);
        ref.invalidate(albumsProvider);
        album = all.albums.where((a) => a.name == name).lastOrNull;
      } on Object catch (e) {
        said('$e');
        return;
      }
      if (album == null) return;
    }
    said(await n.setAlbumMembership(album.id, album.name, add: true));
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

Future<String?> _askName(
  BuildContext context, {
  required String title,
  String initial = '',
}) async {
  final c = TextEditingController(text: initial);
  final got = await showDialog<String>(
    context: context,
    builder: (ctx) => AlertDialog(
      title: Text(title),
      content: TextField(
        controller: c,
        autofocus: true,
        decoration: const InputDecoration(hintText: '相册名'),
        onSubmitted: (v) => Navigator.of(ctx).pop(v),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(ctx).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(ctx).pop(c.text),
          child: const Text('好'),
        ),
      ],
    ),
  );
  c.dispose();
  final name = got?.trim();
  return (name == null || name.isEmpty) ? null : name;
}

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
