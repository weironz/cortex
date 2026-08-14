import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../models/attachment.dart';
import '../../../state/attachment_controller.dart';
import '../../workspace/workspace_panel.dart';
import 'attachment_views.dart';
import 'permission_mode_chip.dart';

/// Input box.
///
/// Enter sends, Shift+Enter inserts a newline — implemented with a
/// [Shortcuts]/[Actions] override rather than by inspecting raw key events, so
/// IME composition (essential for Chinese input) is not interrupted: while a
/// candidate window is open the framework consumes Enter itself and our action
/// never fires.
///
/// Attachments have two entry points, the button and drag-and-drop, and both
/// converge on [AttachmentQueue]: bytes in, a registered blob hash out. Files
/// upload the moment they arrive rather than on send, so the wait happens while
/// the user is still typing instead of after they press Enter.
class MessageComposer extends ConsumerStatefulWidget {
  const MessageComposer({
    super.key,
    required this.onSend,
    required this.onStop,
    required this.streaming,
    this.sessionId,
    this.enabled = true,
    this.centred = false,
  });

  final void Function(String text, List<Attachment> attachments) onSend;
  final VoidCallback onStop;
  final bool streaming;

  /// Attachments are per-session, so the tray needs to know which one.
  final String? sessionId;
  final bool enabled;

  /// 会话还是空的 —— 输入框此刻**站在页面中央**，不是钉在底边。
  ///
  /// 差别不只是位置：一个还没开口的会话里，输入框就是整个页面的主体，
  /// 所以它更高、更大，而且不该画那条把它与「上面的对话」隔开的分隔线 ——
  /// 上面根本没有对话。开口之后它退回底边，让位给正文。
  final bool centred;

  @override
  ConsumerState<MessageComposer> createState() => _MessageComposerState();
}

class _MessageComposerState extends ConsumerState<MessageComposer> {
  final TextEditingController _controller = TextEditingController();
  final FocusNode _focusNode = FocusNode();
  bool _hasText = false;
  bool _dragging = false;

  @override
  void initState() {
    super.initState();
    _controller.addListener(() {
      final has = _controller.text.trim().isNotEmpty;
      if (has != _hasText) setState(() => _hasText = has);
    });
    // The container draws a focus ring, so focus changes must repaint it.
    _focusNode.addListener(() {
      if (mounted) setState(() {});
    });
  }

  @override
  void dispose() {
    _controller.dispose();
    _focusNode.dispose();
    super.dispose();
  }

  void _submit() {
    if (widget.streaming || !widget.enabled) return;
    final sessionId = widget.sessionId;
    final queue = ref.read(attachmentQueueProvider.notifier);
    final attachments = queue.readyFor(sessionId);
    final text = _controller.text.trim();
    if (text.isEmpty && attachments.isEmpty) return;

    _controller.clear();
    if (sessionId != null) queue.clear(sessionId);
    widget.onSend(text, attachments);
    _focusNode.requestFocus();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final sessionId = widget.sessionId;

    // Watched, not read: the send button must light up when an upload finishes
    // even if the text field has not been touched since.
    final tray = ref.watch(attachmentQueueProvider);
    final pending = sessionId == null
        ? const <PendingAttachment>[]
        : (tray[sessionId] ?? const <PendingAttachment>[]);
    final uploading = pending.any((p) => p.status == UploadStatus.uploading);
    final hasReady = pending.any((p) => p.status == UploadStatus.ready);
    // An upload still in flight blocks send: the daemon rejects a hash it has
    // not registered, so sending now would fail the whole turn rather than just
    // drop the attachment.
    final canSend = widget.enabled && !uploading && (_hasText || hasReady);

    return DropTarget(
      enable: widget.enabled && sessionId != null,
      onDragEntered: (_) => setState(() => _dragging = true),
      onDragExited: (_) => setState(() => _dragging = false),
      onDragDone: (detail) {
        setState(() => _dragging = false);
        if (sessionId == null) return;
        ref
            .read(attachmentQueueProvider.notifier)
            .addDropped(sessionId, detail.files);
      },
      child: Container(
        decoration: BoxDecoration(
          color: widget.centred ? Colors.transparent : scheme.surface,
          // 居中形态没有分隔线：它隔开的是「上面的对话」，而此刻上面没有对话
          border: widget.centred
              ? null
              : Border(top: BorderSide(color: scheme.outlineVariant)),
        ),
        padding: widget.centred
            ? const EdgeInsets.fromLTRB(20, 4, 20, 4)
            : const EdgeInsets.fromLTRB(20, 14, 20, 16),
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 820),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              mainAxisSize: MainAxisSize.min,
              children: [
                if (sessionId != null)
                  PendingAttachmentTray(sessionId: sessionId),
                Container(
                  decoration: BoxDecoration(
                    color: _dragging
                        ? scheme.primary.withValues(alpha: 0.06)
                        : scheme.surfaceContainerHigh,
                    borderRadius: BorderRadius.circular(
                      widget.centred ? 18 : 14,
                    ),
                    border: Border.all(
                      color: _dragging || _focusNode.hasFocus
                          ? scheme.primary
                          : scheme.outlineVariant,
                      width: _dragging ? 1.5 : 1,
                    ),
                  ),
                  padding: widget.centred
                      ? const EdgeInsets.fromLTRB(8, 8, 8, 8)
                      : const EdgeInsets.fromLTRB(6, 4, 6, 4),
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.end,
                    children: [
                      Padding(
                        padding: const EdgeInsets.only(bottom: 4),
                        child: IconButton(
                          onPressed: widget.enabled && sessionId != null
                              ? () => ref
                                    .read(attachmentQueueProvider.notifier)
                                    .pickAndUpload(sessionId)
                              : null,
                          iconSize: 19,
                          tooltip: '添加附件（也可以直接拖进来）',
                          icon: const Icon(Icons.attach_file_rounded),
                        ),
                      ),
                      Expanded(
                        child: Shortcuts(
                          shortcuts: const {
                            SingleActivator(LogicalKeyboardKey.enter):
                                _SendIntent(),
                          },
                          child: Actions(
                            actions: {
                              _SendIntent: CallbackAction<_SendIntent>(
                                onInvoke: (_) {
                                  _submit();
                                  return null;
                                },
                              ),
                            },
                            child: TextField(
                              controller: _controller,
                              focusNode: _focusNode,
                              enabled: widget.enabled,
                              maxLines: 8,
                              // 居中形态给三行的高度：这时它是页面的主体，
                              // 一行高的输入框在一整屏留白里像个搜索框
                              minLines: widget.centred ? 3 : 1,
                              keyboardType: TextInputType.multiline,
                              textInputAction: TextInputAction.newline,
                              style: theme.textTheme.bodyLarge,
                              onTapOutside: (_) {},
                              decoration: InputDecoration(
                                filled: false,
                                border: InputBorder.none,
                                enabledBorder: InputBorder.none,
                                focusedBorder: InputBorder.none,
                                contentPadding: const EdgeInsets.symmetric(
                                  vertical: 12,
                                ),
                                hintText: widget.enabled
                                    ? _dragging
                                          ? '松手即可添加附件'
                                          : '问点什么…（Enter 发送，Shift+Enter 换行）'
                                    : '先选择或新建一个会话',
                              ),
                            ),
                          ),
                        ),
                      ),
                      const SizedBox(width: 6),
                      Padding(
                        padding: const EdgeInsets.only(bottom: 4),
                        child: widget.streaming
                            ? _RoundButton(
                                onPressed: widget.onStop,
                                tooltip: '停止生成',
                                icon: Icons.stop_rounded,
                                background: scheme.surfaceContainerHighest,
                                foreground: scheme.onSurface,
                              )
                            : _RoundButton(
                                onPressed: canSend ? _submit : null,
                                tooltip: uploading ? '附件还在上传' : '发送',
                                icon: Icons.arrow_upward_rounded,
                                background: canSend
                                    ? scheme.primary
                                    : scheme.surfaceContainerHighest,
                                foreground: canSend
                                    ? scheme.onPrimary
                                    : scheme.onSurfaceVariant,
                              ),
                      ),
                    ],
                  ),
                ),
                const SizedBox(height: 7),
                Row(
                  children: [
                    // 「这轮的文件去哪儿」与「谁来把关」是同一类东西：
                    // 发出去**之前**要定的事。所以它们贴着输入框，而不是
                    // 挂在顶栏 —— 顶栏那一排是应用级的显示开关，混在一起
                    // 会让人以为工作区也是「看不看」而不是「跑在哪」
                    const _BeforeSendChips(),
                    Expanded(
                      child: Text(
                        'Cortex 会把这轮对话归档，并从中抽取可追溯的记忆。',
                        textAlign: TextAlign.center,
                        style: theme.textTheme.labelSmall,
                      ),
                    ),
                    // 与左边那组等宽的占位，让中间那句话真的居中。
                    // 不这么做的话它会被推得偏右，而那行字是**居中**
                    // 才读得像一句说明、而不是某个控件的标签
                    const Opacity(
                      opacity: 0,
                      child: IgnorePointer(child: _BeforeSendChips()),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// 发送**之前**要定的两件事：这轮跑在哪个目录、谁来把关。
///
/// 单开一个 widget 是因为它要被画两遍 —— 一遍真的，一遍透明的占位，
/// 好让中间那句说明居中。两处必须**同宽**，所以必须是同一个东西。
class _BeforeSendChips extends StatelessWidget {
  const _BeforeSendChips();

  @override
  Widget build(BuildContext context) => const Row(
    mainAxisSize: MainAxisSize.min,
    children: [
      // 工作区在前：它决定文件落在哪，是两者里更容易选错、
      // 也更难在事后发现选错了的那一个
      WorkspaceChip(),
      SizedBox(width: 6),
      PermissionModeChip(),
    ],
  );
}

class _SendIntent extends Intent {
  const _SendIntent();
}

class _RoundButton extends StatelessWidget {
  const _RoundButton({
    required this.onPressed,
    required this.tooltip,
    required this.icon,
    required this.background,
    required this.foreground,
  });

  final VoidCallback? onPressed;
  final String tooltip;
  final IconData icon;
  final Color background;
  final Color foreground;

  @override
  Widget build(BuildContext context) {
    return Tooltip(
      message: tooltip,
      child: Material(
        color: background,
        shape: const CircleBorder(),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: onPressed,
          child: SizedBox(
            width: 34,
            height: 34,
            child: Icon(icon, size: 18, color: foreground),
          ),
        ),
      ),
    );
  }
}
