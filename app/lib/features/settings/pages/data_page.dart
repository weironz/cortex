import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/local_agent.dart';
import '../../../core/save_file.dart';
import '../../../core/theme.dart';
import '../../../state/app_providers.dart';
import '../../../workspace/workspace_fs.dart';
import '../../import/import_sheet.dart';
import '../widgets/settings_layout.dart';

/// 数据这一页：文件落在哪、以及从别处搬进来。
class DataPage extends StatelessWidget {
  const DataPage({super.key});

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.symmetric(horizontal: 4),
      children: const [WorkspaceSection(), _ImportSection(), _ExportSection()],
    );
  }
}

/// 工作空间落在哪。
class WorkspaceSection extends ConsumerWidget {
  const WorkspaceSection({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    // Web 上没有本机目录可言：那边的 agent 在云端容器里，工作区是它自己的
    // `/workspace`。判据用能力而不是 kIsWeb，与 WorkspaceChip 同一个
    if (!kLocalAgentSupported) return const SizedBox.shrink();

    final async = ref.watch(localWorkspaceRootProvider);
    final root = async.value?.root;

    return SettingsSection(
      title: '工作空间',
      description: '新建对话时会在这里建文件夹。改了不影响已有数据 —— 老会话仍指向它们各自建好的目录。',
      trailing: TextButton(
        onPressed: root == null ? null : () => _change(context, ref),
        child: const Text('更改'),
      ),
      children: [
        SettingsCard(
          child: SettingsRow(
            label: '默认目录',
            // 路径要逐字符看（哪一层拼错了），所以等宽
            monospace: root != null,
            value: async.isLoading
                ? '读取中…'
                : root ?? '本地 agent 没在跑，读不到这台机器上的设置',
          ),
        ),
      ],
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

/// 从 ChatGPT / Claude 搬历史进来。
class _ImportSection extends StatelessWidget {
  const _ImportSection();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return SettingsSection(
      title: '导入',
      // 放这里而不是工具栏：导入是一个人做一次的事。一个常驻按钮配一个
      // 一次性动作是噪音，而「数据」正是他会来找它的地方
      description: '把别处的对话历史搬进来。会先把要花多少钱摊开，确认之后才开始写。',
      children: [
        SettingsCard(
          child: Row(
            children: [
              Expanded(
                child: Text(
                  '选择导出包里的 conversations.json',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.onSurface,
                  ),
                ),
              ),
              const SizedBox(width: 12),
              FilledButton.tonalIcon(
                onPressed: () {
                  // 先关掉设置窗。把一个能跑一刻钟的进度条叠在设置上面，
                  // 会留下一层再也点不到的遮罩
                  Navigator.of(context).pop();
                  unawaited(showImportSheet(context));
                },
                icon: const Icon(Icons.download_outlined, size: 16),
                label: const Text('选择文件'),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// 把自己的会话带走。
///
/// # 为什么必须有
///
/// 一个以「永不丢失」为承诺的产品，拿不走自己的数据是自相矛盾的。
/// 这也是 GDPR 的可携带权。
///
/// # ⚠️ 这里必须说清「记忆不在这份文件里」
///
/// 拆分之后长期记忆整个在 Cormex，这一侧一列都没有。一个写着
/// 「导出我的数据」的按钮如果只给一半而不说，用户会得出**另一半不存在**
/// 这个结论 —— 而那正是 CLAUDE.md 约束 2 说的那种「说得到做不到」，
/// 只不过方向反过来：这次是把做得到的事说小了，代价一样是骗人。
class _ExportSection extends ConsumerStatefulWidget {
  const _ExportSection();

  @override
  ConsumerState<_ExportSection> createState() => _ExportSectionState();
}

class _ExportSectionState extends ConsumerState<_ExportSection> {
  bool _busy = false;

  Future<void> _export() async {
    setState(() => _busy = true);
    Uint8List? bytes;
    String? error;
    try {
      bytes = await ref.read(cortexApiProvider).exportSessions();
    } on CortexApiException catch (e) {
      error = e.message;
    } on Object catch (e) {
      error = '$e';
    }
    if (!mounted) return;
    setState(() => _busy = false);

    if (bytes != null) {
      final saved = await saveBytesAs(
        bytes,
        'cortex-sessions.ndjson',
        mimeType: 'application/x-ndjson',
      );
      if (!mounted) return;
      if (saved == null) return; // 用户自己取消了，不该弹提示
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(SnackBar(content: Text('已存到 $saved')));
      return;
    }
    if (!mounted) return;
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(SnackBar(content: Text(error ?? '导出失败')));
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return SettingsSection(
      title: '导出',
      // ⚠️ 必须点明「单条会话在别处」。左栏右键早就有一条按会话导出
      //（Markdown / JSON，见 core/session_export.dart），而这里这条是
      // **整个账号**。不说清的话，用户会以为其中一条多余，
      // 或者为了拿一段对话把整个库导一遍
      description:
          '把全部对话存成一份文件带走。一行一条记录（NDJSON），'
          '任何文本编辑器都打得开。只要某一段对话的话，'
          '在左栏右键那条会话 → 导出，还能选 Markdown。',
      children: [
        SettingsCard(
          child: Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      '对话、标题、时间、模型都在里面',
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: theme.colorScheme.onSurface,
                      ),
                    ),
                    const SizedBox(height: 2),
                    // ⚠️ **只说文件里有什么，不说没有什么。**
                    //
                    // 这里第一版写的是「长期记忆不在这份文件里 —— 去记忆
                    // 服务那边导」，被 `no_stale_memory_copy_test` 拦下了，
                    // 而它拦得比我想的更对：这个产品**现在根本没有长期
                    // 记忆**（三条路 2026-08-17 全拆了，见 CLAUDE.md）。
                    // 那句话会让用户去找一个不存在的入口 —— 与「承诺一个
                    // 不存在的功能」是同一个错，只是绕了个弯。
                    Text(
                      '附件只给引用（要原件去对话里下载）。'
                      '这份文件里的是对话本身。',
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: theme.cortex.foregroundTertiary,
                        height: 1.5,
                      ),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 12),
              FilledButton.tonalIcon(
                onPressed: _busy ? null : () => unawaited(_export()),
                icon: _busy
                    ? const SizedBox(
                        width: 14,
                        height: 14,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.upload_outlined, size: 16),
                label: Text(_busy ? '正在导出…' : '导出'),
              ),
            ],
          ),
        ),
      ],
    );
  }
}
