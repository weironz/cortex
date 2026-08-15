import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/local_agent.dart';
import '../../../state/app_providers.dart';
import '../../../workspace/workspace_fs.dart';
import '../../import/import_sheet.dart';

/// 数据这一页：文件落在哪、以及从别处搬进来。
class DataPage extends StatelessWidget {
  const DataPage({super.key});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.symmetric(horizontal: 4),
      children: [
        const WorkspaceRootTile(),
        // 放这里而不是工具栏：导入是一个人做一次的事。一个常驻按钮配一个
        // 一次性动作是噪音，而「数据」正是他会来找它的地方
        ListTile(
          contentPadding: EdgeInsets.zero,
          leading: const Icon(Icons.download_outlined),
          title: const Text('导入 ChatGPT / Claude 历史'),
          subtitle: Text(
            '选择导出包里的 conversations.json。会先把要花多少钱摊开，'
            '确认之后才开始写。',
            style: theme.textTheme.bodySmall,
          ),
          onTap: () {
            // 先关掉设置窗。把一个能跑一刻钟的进度条叠在设置上面，
            // 会留下一层再也点不到的遮罩
            Navigator.of(context).pop();
            unawaited(showImportSheet(context));
          },
        ),
      ],
    );
  }
}

class WorkspaceRootTile extends ConsumerWidget {
  const WorkspaceRootTile({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    // Web 上没有本机目录可言：那边的 agent 在云端容器里，工作区是它自己的
    // `/workspace`。判据用能力而不是 kIsWeb，与 WorkspaceChip 同一个
    if (!kLocalAgentSupported) return const SizedBox.shrink();

    final theme = Theme.of(context);
    final async = ref.watch(localWorkspaceRootProvider);
    final root = async.value?.root;

    return ListTile(
      contentPadding: EdgeInsets.zero,
      leading: const Icon(Icons.folder_special_outlined),
      title: const Text('默认工作空间'),
      subtitle: Text(
        async.isLoading
            ? '读取中…'
            : root == null
            ? '本地 agent 没在跑，读不到这台机器上的设置。'
            : '$root\n新建对话时会在这里建文件夹；改了不影响已有数据。',
        style: theme.textTheme.bodySmall,
      ),
      isThreeLine: root != null,
      trailing: TextButton(
        onPressed: root == null ? null : () => _change(context, ref),
        child: const Text('更改'),
      ),
    );
  }

  Future<void> _change(BuildContext context, WidgetRef ref) async {
    final picked = await pickWorkspaceDirectory();
    if (picked == null || !context.mounted) return;
    try {
      await ref.read(cortexApiProvider).setLocalWorkspaceRoot(picked);
      ref.invalidate(localWorkspaceRootProvider);
    } on CortexApiException catch (e) {
      if (!context.mounted) return;
      // 校验器会拒掉主目录本身与系统目录，理由是写给人看的，原样显示
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text(e.message)));
    }
  }
}

