/// 「移动到哪个文件夹」这个选择器 —— **批量与单张共用一份**。
///
/// # 为什么抽出来
///
/// 它原本长在多选工具条里（`folder_bar` 的 `_addTo`）。2026-08-27 给单张
/// 图的右键菜单补「移动到文件夹…」时，第二处要的是**一模一样**的东西：
/// 列出文件夹、空清单说句人话、末尾一个「新建…」、建完直接用新建的那个。
///
/// 抄一份的下场是这四个细节各自演化 —— 尤其是「建完直接用它」那一步：
/// 漏掉的话用户建了个文件夹，然后发现什么也没发生，得再点一次。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../models/generated_image.dart';
import '../../../state/app_providers.dart';
import '../../../state/image_controller.dart';

/// 让用户挑一个文件夹（可以当场新建）。
///
/// 返回挑中的那个；用户放弃时返回 `null`。
/// 出错时用 [said] 说一句并返回 `null` —— 静默失败会让「移动」变成
/// 「点了没反应」。
Future<Folder?> pickFolder(
  BuildContext context,
  WidgetRef ref, {
  required void Function(String message) said,
  String title = '移动到哪个文件夹',
}) async {
  final folders = ref.read(foldersProvider).value?.folders ?? const <Folder>[];
  final picked = await showModalBottomSheet<Folder>(
    context: context,
    builder: (ctx) => SafeArea(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 14, 16, 6),
            child: Align(alignment: Alignment.centerLeft, child: Text(title)),
          ),
          if (folders.isEmpty)
            const Padding(
              padding: EdgeInsets.fromLTRB(16, 4, 16, 12),
              child: Align(
                alignment: Alignment.centerLeft,
                // 一个空列表什么都不说的话，用户会以为这个功能坏了
                child: Text('还没有文件夹 —— 下面新建一个。'),
              ),
            ),
          for (final f in folders)
            ListTile(
              key: ValueKey('moveto:${f.id}'),
              leading: const Icon(Icons.folder_outlined, size: 20),
              title: Text(f.name),
              subtitle: Text('${f.count} 项'),
              onTap: () => Navigator.of(ctx).pop(f),
            ),
          ListTile(
            key: const ValueKey('moveto:new'),
            leading: const Icon(Icons.create_new_folder_outlined, size: 20),
            title: const Text('新建文件夹…'),
            onTap: () => Navigator.of(ctx).pop(),
          ),
        ],
      ),
    ),
  );
  if (!context.mounted) return null;
  if (picked != null) return picked;

  // 上面那个 sheet 点外面关掉时也是 null。所以这里再问一次名字：
  // 用户按 Esc 取消掉这一步，才是真的放弃
  final name = await askFolderName(context, title: '新建文件夹');
  if (name == null) return null;
  try {
    final all = await ref.read(cortexApiProvider).createFolder(name);
    ref.invalidate(foldersProvider);
    // 建完**直接用它**，不让用户再点一次选择器 —— 他刚打了那个名字，
    // 意图明摆着
    return all.folders.where((f) => f.name == name).lastOrNull;
  } on Object catch (e) {
    said('$e');
    return null;
  }
}

/// 问一个文件夹名字。`null` = 用户放弃。
Future<String?> askFolderName(
  BuildContext context, {
  required String title,
  String initial = '',
}) async {
  final controller = TextEditingController(text: initial);
  final name = await showDialog<String>(
    context: context,
    builder: (ctx) => AlertDialog(
      title: Text(title),
      content: TextField(
        controller: controller,
        autofocus: true,
        maxLength: 64,
        decoration: const InputDecoration(hintText: '文件夹名'),
        onSubmitted: (v) => Navigator.of(ctx).pop(v),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(ctx).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(ctx).pop(controller.text),
          child: const Text('好'),
        ),
      ],
    ),
  );
  final trimmed = name?.trim() ?? '';
  return trimmed.isEmpty ? null : trimmed;
}
