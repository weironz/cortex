/// 一个项目上能做的事 —— **左栏那一行与项目页那张卡共用这一份**。
///
/// # 为什么必须是同一份
///
/// 与 `image_actions.dart` 同一个理由：两处各写一遍的下场是「在项目页能
/// 改名，在左栏不能」，而用户根本分不清那是两个功能还是一个 bug。
///
/// 这些函数原本长在 `session_list.dart` 里（只有左栏用得着）。项目页进来
/// 之后它们有了第二个调用方，所以搬到这儿。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api_exception.dart';
import '../../models/project.dart';
import '../../state/project_controller.dart';

/// 新建一个项目。返回建好的那个；用户取消或失败时是 `null`。
Future<Project?> createProject(BuildContext context, WidgetRef ref) async {
  final name = await askProjectName(
    context,
    title: '新建项目',
    hint: '例如：Cortex 客户端',
  );
  if (name == null || !context.mounted) return null;
  Project? made;
  await runProjectAction(context, () async {
    made = await ref.read(projectControllerProvider.notifier).create(name);
  });
  return made;
}

Future<void> renameProject(
  BuildContext context,
  WidgetRef ref,
  Project project,
) async {
  final name = await askProjectName(
    context,
    title: '重命名项目',
    initial: project.name,
  );
  if (name == null || !context.mounted) return;
  await runProjectAction(
    context,
    () => ref.read(projectControllerProvider.notifier).rename(project.id, name),
  );
}

/// 置顶 / 取消置顶。
///
/// 控制器那边会**核对服务端真的改了** —— 老服务端静默忽略 `pinned` 并回
/// 200，不核对的话界面上那一项当场移过去、刷新又弹回来。它回一句话就在
/// 这里原样说出去。
Future<void> setProjectPinned(
  BuildContext context,
  WidgetRef ref,
  Project project,
  bool pinned,
) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  final said = await ref
      .read(projectControllerProvider.notifier)
      .setPinned(project.id, pinned);
  if (said != null) {
    messenger?.showSnackBar(SnackBar(content: Text(said)));
  }
}

/// 删除项目。
///
/// 对话框正文由 [deleteProjectWarning] 写死，且**必须**说清会话不会丢：
/// 用户在这里唯一真正害怕的事就是「删项目把对话也删了」，而那恰恰是唯一
/// 不会发生的事。说不清楚的代价不是一次误操作，是从此没人敢碰这个功能。
Future<void> deleteProject(
  BuildContext context,
  WidgetRef ref,
  Project project,
) async {
  final ok = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text('删除项目「${project.name}」？'),
      content: Text(deleteProjectWarning(project)),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(true),
          // 「删除项目」而不是「删除」：按钮上那个宾语是最后一次说明
          // 被删掉的到底是什么
          child: const Text('删除项目'),
        ),
      ],
    ),
  );
  if (ok != true || !context.mounted) return;
  await runProjectAction(
    context,
    () => ref.read(projectControllerProvider.notifier).remove(project.id),
  );
}

Future<String?> askProjectName(
  BuildContext context, {
  required String title,
  String? initial,
  String? hint,
}) async {
  final field = TextEditingController(text: initial ?? '');
  final result = await showDialog<String>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(title),
      content: TextField(
        controller: field,
        autofocus: true,
        // 服务端 trim 之后限 100 字符。在这里就截住，是为了不让用户
        // 打完一长串再收到一条他看不出所以然的 400
        maxLength: 100,
        buildCounter:
            (_, {required currentLength, required isFocused, maxLength}) =>
                null,
        decoration: InputDecoration(labelText: '项目名', hintText: hint),
        onSubmitted: (v) => Navigator.of(context).pop(v),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(field.text),
          child: const Text('保存'),
        ),
      ],
    ),
  );
  field.dispose();
  if (result == null || result.trim().isEmpty) return null;
  return result.trim();
}

/// 跑一个会打网络的动作，失败时把服务端那句话**原样**说出去。
///
/// 服务端的消息比我们能编的具体（「项目名不能为空白」「找不到 project」），
/// 换成一句自己的话只会丢信息。
Future<void> runProjectAction(
  BuildContext context,
  Future<void> Function() action,
) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  try {
    await action();
  } on CortexApiException catch (e) {
    messenger?.showSnackBar(SnackBar(content: Text(e.message)));
  } on Object catch (e) {
    messenger?.showSnackBar(SnackBar(content: Text('$e')));
  }
}
