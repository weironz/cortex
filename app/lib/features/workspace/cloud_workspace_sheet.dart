import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api_exception.dart';
import '../../api/cortex_api.dart' show kSandboxRoot;
import '../../models/chat_session.dart';
import '../../state/app_providers.dart';
import '../../state/chat_controller.dart';

/// 云沙箱工作区的名字规则。
///
/// **与三处是同一份规则**：库里那条 `container_workspace ~ '…'` 的 CHECK、
/// `cortex-local` 的 `container_subdir`、以及 mock 后端。这里再写一遍不是
/// 重复校验，是为了让一个明显不合格的名字在**按下按钮之前**就被指出来 ——
/// 让用户等一次往返才知道「不能有斜杠」是没必要的。
///
/// 服务端仍然是权威：这里放行的名字它照样可以拒，而它那句话原样显示。
final RegExp kContainerWorkspaceShape = RegExp(
  r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$',
);

/// 打开「选云端工作区」。
///
/// # 这是云端那一支的「更换工作区」
///
/// 桌面端那一支是 [showWorkspaceBindingSheet]（绑本机的一个绝对路径）。
/// 两者刻意分成两个界面，因为它们回答的**不是同一个问题**：
/// 那边问「用这台机器上的哪个目录」，这边问「用云端那个卷里的哪个子目录」。
/// 合成一个的话，界面上要先让用户回答一个他不该关心的问题（这一轮跑在哪儿）。
///
/// [showWorkspaceBindingSheet]: workspace_binding_sheet.dart
Future<void> showCloudWorkspaceSheet(
  BuildContext context,
  ChatSession session,
) => showDialog<void>(
  context: context,
  builder: (_) => _CloudWorkspaceDialog(session: session),
);

class _CloudWorkspaceDialog extends ConsumerStatefulWidget {
  const _CloudWorkspaceDialog({required this.session});

  final ChatSession session;

  @override
  ConsumerState<_CloudWorkspaceDialog> createState() =>
      _CloudWorkspaceDialogState();
}

class _CloudWorkspaceDialogState extends ConsumerState<_CloudWorkspaceDialog> {
  /// 卷根下已有的子目录名。null = 还在拉。
  List<String>? _existing;

  /// 列不出来时的原因。**不致命** —— 输入框那条路照样能用，
  /// 所以这里只显示一句话，不把整个对话框变成错误页。
  String? _listError;

  late String? _selected = widget.session.containerWorkspace;
  final TextEditingController _newName = TextEditingController();
  String? _error;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    unawaited(_loadExisting());
  }

  @override
  void dispose() {
    _newName.dispose();
    super.dispose();
  }

  /// 列出卷根下有哪些子目录。
  ///
  /// 走 `GET /sandbox/files?path=/workspace` —— 那条路按**容器**取目录、
  /// 不按会话当前的根，所以一个已经收窄到 `client-a` 的会话也看得见它的
  /// 兄弟目录。这正是「选择器」比「输入框」强的地方：用户不必记得名字。
  Future<void> _loadExisting() async {
    try {
      final entries = await ref
          .read(cortexApiProvider)
          .sandboxListFiles(kSandboxRoot, sessionId: widget.session.id);
      if (!mounted) return;
      setState(() {
        _existing = [
          for (final e in entries)
            if (e.isDirectory) e.name,
        ]..sort();
      });
    } on CortexApiException catch (e) {
      if (!mounted) return;
      // 沙箱还没起来（409）是最常见的一种，而那**不是错误**：
      // 这个用户只是还没在云端跑过。此时新建一个名字照样合法
      setState(() {
        _existing = const [];
        _listError = e.message;
      });
    } on Object catch (e) {
      if (!mounted) return;
      setState(() {
        _existing = const [];
        _listError = '$e';
      });
    }
  }

  Future<void> _apply(String? name) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await ref
          .read(chatControllerProvider.notifier)
          .setContainerWorkspace(widget.session.id, name);
      if (mounted) Navigator.of(context).pop();
    } on CortexApiException catch (e) {
      // 服务端的校验器是写给用户看的，原样显示
      if (mounted) setState(() => _error = e.message);
    } on Object catch (e) {
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _createAndApply() async {
    final name = _newName.text.trim();
    if (name.isEmpty) {
      setState(() => _error = '请填一个名字');
      return;
    }
    if (!kContainerWorkspaceShape.hasMatch(name)) {
      setState(
        () => _error =
            '只允许字母数字与 . _ -，必须以字母或数字开头，最长 64 —— '
            '不能有斜杠：它是一段目录名，不是路径',
      );
      return;
    }
    await _apply(name);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final existing = _existing;

    return AlertDialog(
      title: const Text('云端工作区'),
      contentPadding: const EdgeInsets.fromLTRB(24, 16, 24, 0),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                // 这句话是这个界面最要紧的一行：不说的话，用户会以为
                // 选一个名字等于「给这个会话开一个私有目录」，
                // 然后在另一个会话里找不到文件时以为丢了
                // 「同名共用」这件事要显眼，但 `Text` 不渲染 Markdown ——
                // 加粗只能靠排版（单独成句 + 引号），不能靠星号。
                // 仓库里有一条测试专门抓字面星号，它当场抓住过这一行
                '选一个目录，agent 就只在它里面读写。'
                '「同名的会话共用同一个目录」—— 这不是「一个会话一个目录」：'
                '按会话分的话，「昨天让你生成的那份报告呢」会得到一个空目录。',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
              const SizedBox(height: 16),

              // `RadioGroup` 而不是每个 tile 各带 groupValue/onChanged ——
              // 后者在这个 Flutter 版本已废弃，而 CI 的 analyze 把 info 也当红
              RadioGroup<String?>(
                groupValue: _selected,
                // `RadioGroup.onChanged` 不接受 null（不像单个 Radio），
                // 所以「正在提交时不许改」在回调里判，而不是把回调置空
                onChanged: (v) {
                  if (_busy) return;
                  setState(() => _selected = v);
                },
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    // ── 卷根 ──
                    RadioListTile<String?>(
                      value: null,
                      contentPadding: EdgeInsets.zero,
                      title: const Text('整个卷（默认）'),
                      subtitle: Text(
                        '/workspace —— 这个项目下所有会话共用',
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    ),
                    // ── 已有的子目录 ──
                    if (existing == null)
                      const Padding(
                        padding: EdgeInsets.symmetric(vertical: 12),
                        child: LinearProgressIndicator(minHeight: 2),
                      )
                    else
                      for (final name in existing)
                        RadioListTile<String?>(
                          value: name,
                          contentPadding: EdgeInsets.zero,
                          title: Text(name),
                          subtitle: Text(
                            '/workspace/$name',
                            style: theme.textTheme.labelSmall?.copyWith(
                              color: scheme.onSurfaceVariant,
                            ),
                          ),
                        ),
                  ],
                ),
              ),

              if (_listError != null)
                Padding(
                  padding: const EdgeInsets.only(top: 4, bottom: 8),
                  child: Text(
                    // 明说这只影响「列出来」这件事 —— 新建那条路还能走
                    '列不出已有的目录（$_listError）。新建一个仍然可以。',
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                ),

              const Divider(height: 24),

              // ── 新建 ──
              TextField(
                controller: _newName,
                enabled: !_busy,
                decoration: const InputDecoration(
                  labelText: '新建一个',
                  hintText: 'client-a',
                  helperText: '字母数字与 . _ -，不能有斜杠',
                  isDense: true,
                ),
                onSubmitted: (_) => _busy ? null : _createAndApply(),
              ),

              if (_error != null)
                Padding(
                  padding: const EdgeInsets.only(top: 12),
                  child: Text(
                    _error!,
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: scheme.error,
                    ),
                  ),
                ),
              const SizedBox(height: 8),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        TextButton(
          onPressed: _busy || _newName.text.trim().isEmpty
              ? null
              : _createAndApply,
          child: const Text('新建并使用'),
        ),
        FilledButton(
          onPressed: _busy ? null : () => _apply(_selected),
          child: _busy
              ? const SizedBox(
                  width: 14,
                  height: 14,
                  child: CircularProgressIndicator(strokeWidth: 2),
                )
              : const Text('使用选中的'),
        ),
      ],
    );
  }
}
