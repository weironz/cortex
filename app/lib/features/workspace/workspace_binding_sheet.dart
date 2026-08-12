import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api_exception.dart';
import '../../models/chat_session.dart';
import '../../state/chat_controller.dart';
import '../../workspace/workspace_fs.dart';

/// Binds (or rebinds) a session's workspace.
///
/// ## The two platforms get different affordances, not different features
///
/// Desktop opens the OS folder chooser. Web cannot — and the reason is worth
/// stating precisely, because the obvious guesses are both wrong:
///
/// * It is *not* that browsers cannot pick directories. Chromium has
///   `showDirectoryPicker`, and it would be easy enough to reach through
///   `dart:js_interop`.
/// * It is *not* that Flutter Web lacks a plugin for it.
///
/// It is that **the path has to be meaningful to cortexd, not to the browser.**
/// The agent's file tools run inside the daemon and are fenced by
/// `cortex_agent::Sandbox`; a workspace is an absolute path on the daemon's
/// filesystem. A browser directory handle names a folder on the *user's*
/// machine, which in a Web deployment is usually not the daemon's machine at
/// all — and even when it is, the handle cannot be turned back into a path.
/// Wiring up `showDirectoryPicker` would produce a picker that looks like it
/// works and binds nothing usable.
///
/// So Web gets a path field instead, with the reason written on screen and the
/// daemon's own validator as the safety net: an absolute path that exists, is a
/// directory, is not a filesystem root, a system directory, or the home
/// directory itself. Its rejections are shown verbatim — they are written to be
/// read by a person, and rewording them here would only make them vaguer.
///
/// The one thing this must never be is a button that does nothing when clicked.
Future<void> showWorkspaceBindingSheet(
  BuildContext context,
  ChatSession session,
) => showDialog<void>(
  context: context,
  builder: (_) => _WorkspaceBindingDialog(session: session),
);

class _WorkspaceBindingDialog extends ConsumerStatefulWidget {
  const _WorkspaceBindingDialog({required this.session});

  final ChatSession session;

  @override
  ConsumerState<_WorkspaceBindingDialog> createState() =>
      _WorkspaceBindingDialogState();
}

class _WorkspaceBindingDialogState
    extends ConsumerState<_WorkspaceBindingDialog> {
  late final TextEditingController _path = TextEditingController(
    text: widget.session.workspace?.root ?? '',
  );
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _path.dispose();
    super.dispose();
  }

  Future<void> _browse() async {
    final picked = await pickWorkspaceDirectory();
    if (picked == null || !mounted) return;
    setState(() {
      _path.text = picked;
      _error = null;
    });
  }

  Future<void> _submit() async {
    final value = _path.text.trim();
    if (value.isEmpty) {
      setState(() => _error = '请填写或选择一个目录');
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await ref
          .read(chatControllerProvider.notifier)
          .bindWorkspace(widget.session.id, value);
      if (mounted) Navigator.of(context).pop();
    } on CortexApiException catch (e) {
      // The daemon's validator writes for the user; pass it through.
      if (mounted) setState(() => _error = e.message);
    } on Object catch (e) {
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _unbind() async {
    setState(() => _busy = true);
    try {
      await ref
          .read(chatControllerProvider.notifier)
          .unbindWorkspace(widget.session.id);
      if (mounted) Navigator.of(context).pop();
    } on CortexApiException catch (e) {
      if (mounted) setState(() => _error = e.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final bound = widget.session.workspace != null;

    return AlertDialog(
      title: Text(bound ? '更换工作区' : '绑定工作区'),
      contentPadding: const EdgeInsets.fromLTRB(24, 16, 24, 0),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '绑定后，这个会话的助手可以读写该目录里的文件。'
                '未绑定的会话拿不到文件工具 —— 它会直接说自己读不了文件，'
                '而不是试几轮再放弃。',
                style: theme.textTheme.bodySmall,
              ),
              const SizedBox(height: 14),
              TextField(
                controller: _path,
                autofocus: !kCanBrowseLocalFiles,
                decoration: InputDecoration(
                  labelText: 'cortexd 所在机器上的绝对路径',
                  hintText: r'D:\codes\cortex 或 /home/me/work/cortex',
                  errorText: _error,
                  errorMaxLines: 6,
                  suffixIcon: kCanBrowseLocalFiles
                      ? IconButton(
                          onPressed: _busy ? null : _browse,
                          tooltip: '浏览…',
                          icon: const Icon(Icons.folder_open_rounded),
                        )
                      : null,
                ),
                onSubmitted: (_) => _submit(),
              ),
              const SizedBox(height: 12),
              if (!kCanBrowseLocalFiles)
                _Note(icon: Icons.public_rounded, text: kNoLocalFilesReason)
              else
                _Note(
                  icon: Icons.dns_outlined,
                  text:
                      '路径是 cortexd 那台机器上的。桌面端通常与 daemon 同机，'
                      '直接选目录即可；连的是远端 daemon 时，请填那台机器上的路径。',
                ),
              const SizedBox(height: 10),
              Text(
                '会被拒绝的：相对路径、不存在的路径、文件、盘符根目录、'
                '系统目录，以及主目录本身'
                '（`~/.ssh`、`~/.aws`、各处 `.env` 都在里面，'
                '挡的是范围不是位置 —— `~/work` 这样的子目录一律放行）。',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ],
          ),
        ),
      ),
      actions: [
        if (bound)
          TextButton(
            onPressed: _busy ? null : _unbind,
            child: Text('解除绑定', style: TextStyle(color: scheme.error)),
          ),
        TextButton(
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('取消'),
        ),
        FilledButton(
          onPressed: _busy ? null : _submit,
          child: _busy
              ? const SizedBox(
                  width: 15,
                  height: 15,
                  child: CircularProgressIndicator(strokeWidth: 2),
                )
              : const Text('绑定'),
        ),
      ],
    );
  }
}

class _Note extends StatelessWidget {
  const _Note({required this.icon, required this.text});

  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      padding: const EdgeInsets.all(11),
      decoration: BoxDecoration(
        color: scheme.secondary.withValues(alpha: 0.06),
        borderRadius: BorderRadius.circular(9),
        border: Border.all(color: scheme.secondary.withValues(alpha: 0.25)),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(icon, size: 15, color: scheme.secondary),
          const SizedBox(width: 9),
          Expanded(child: Text(text, style: theme.textTheme.labelSmall)),
        ],
      ),
    );
  }
}
