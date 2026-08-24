import 'package:desktop_drop/desktop_drop.dart';
import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:pasteboard/pasteboard.dart';

import '../../../models/attachment.dart';
import '../../../state/attachment_controller.dart';
import '../../../state/composer_draft.dart';
import '../../../state/file_mention_controller.dart';
import '../../workspace/workspace_panel.dart';
import 'attachment_views.dart';
import 'assistant_chip.dart';
import 'model_chip.dart';
import 'message_bubble.dart';
import 'permission_mode_chip.dart';
import '../../../core/theme.dart';

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
    required this.ensureSession,
    this.sessionId,
    this.enabled = true,
    this.centred = false,
  });

  final void Function(String text, List<Attachment> attachments) onSend;
  final VoidCallback onStop;
  final bool streaming;

  /// Attachments are per-session, so the tray needs to know which one.
  ///
  /// **`null` 是白纸状态**（还没开口，会话还没建出来），不是「坏了」。
  final String? sessionId;

  /// 把白纸兑现成一条真会话，返回它的 id。
  ///
  /// # ⚠️ 为什么附件入口需要它
  ///
  /// 附件托盘按 sessionId 存，而惰性建会话之后白纸上没有 id。这一处原先
  /// 的做法是**整个禁用**（`enable: … && sessionId != null`），在从前是对的
  /// ——那时无会话意味着「你还没选中任何东西」。现在它意味着「新对话」，
  /// 于是同一段代码变成了「新对话里不能拖图进来」。
  ///
  /// 拖一张图进来**本身就是开口**，所以这里兑现会话是合理的：用户抱怨的
  /// 是「点开又走开留下空壳」，不是这个。
  final String Function() ensureSession;

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

  /// 正在打的那个 `@…`：`(起点下标, 已经打了的片段)`。null = 没在引用文件。
  ///
  /// 记起点而不是每次重新扫一遍：插入时要把 `@片段` 整段换掉，
  /// 而用户可能在这中间又移动过光标。
  (int, String)? _mention;

  /// 候选列表里高亮的那一条。
  int _mentionAt = 0;

  @override
  void initState() {
    super.initState();
    _controller.addListener(() {
      final has = _controller.text.trim().isNotEmpty;
      if (has != _hasText) setState(() => _hasText = has);
      _syncMention();
    });
    // The container draws a focus ring, so focus changes must repaint it.
    _focusNode.addListener(() {
      if (mounted) setState(() {});
    });
    // 「改一改再发一次」把原话塞进来。用 listenManual 而不是在 build 里
    // watch：写 `_controller.text = …` 属于副作用，放 build 里会在每次
    // 重建时把用户正在打的字冲掉
    ref.listenManual(composerDraftProvider, (_, draft) {
      if (draft == null || !mounted) return;
      _controller.text = draft.text;
      // 光标停在末尾。默认停在开头的话，用户接着敲的字会插在最前面 ——
      // 而他要做的通常是在后面补一句
      _controller.selection = TextSelection.collapsed(
        offset: draft.text.length,
      );
      _focusNode.requestFocus();
      ref.read(composerDraftProvider.notifier).consume();
    });
  }

  @override
  void dispose() {
    _controller.dispose();
    _focusNode.dispose();
    super.dispose();
  }

  /// 光标前面那段有没有正在打的 `@…`。
  ///
  /// 只认**词首**的 `@`（行首或前面是空白）：邮箱地址、`user@host`、
  /// Dart 里的注解都带 `@`，把它们都弹出一个文件列表来会让输入框没法用。
  void _syncMention() {
    final sel = _controller.selection;
    if (!sel.isValid || !sel.isCollapsed) {
      _setMention(null);
      return;
    }
    final text = _controller.text;
    final caret = sel.baseOffset.clamp(0, text.length);
    var i = caret - 1;
    while (i >= 0) {
      final c = text[i];
      if (c == '@') break;
      // 片段里不允许空白：`@` 之后敲了空格就当这次引用结束了
      if (c.trim().isEmpty) {
        _setMention(null);
        return;
      }
      i--;
    }
    if (i < 0) {
      _setMention(null);
      return;
    }
    if (i > 0 && text[i - 1].trim().isNotEmpty) {
      // 前面粘着别的字符 —— 那是 `a@b`，不是一次引用
      _setMention(null);
      return;
    }
    _setMention((i, text.substring(i + 1, caret)));
  }

  void _setMention((int, String)? next) {
    final changed = next?.$1 != _mention?.$1 || next?.$2 != _mention?.$2;
    if (!changed) return;
    setState(() {
      _mention = next;
      _mentionAt = 0;
    });
    // 第一次弹出来才去扫。开着对话就预扫的话，一个几百文件的工作区会在
    // 用户还没打算引用任何东西的时候先卡一下
    if (next != null) {
      unawaited(ref.read(fileMentionProvider.notifier).ensure());
    }
  }

  /// 把 `@片段` 换成 `@完整路径 `。
  void _insertMention(String path) {
    final m = _mention;
    if (m == null) return;
    final text = _controller.text;
    final end = (m.$1 + 1 + m.$2.length).clamp(0, text.length);
    // 末尾补一个空格：不补的话用户接着打的字会粘在路径后面，
    // 而模型看到的是一个不存在的文件名
    final inserted = '@$path ';
    final next = text.replaceRange(m.$1, end, inserted);
    _controller.value = TextEditingValue(
      text: next,
      selection: TextSelection.collapsed(offset: m.$1 + inserted.length),
    );
    _setMention(null);
    _focusNode.requestFocus();
  }

  /// 候选列表当前显示的那几条。
  List<String> _candidates() {
    final m = _mention;
    if (m == null) return const [];
    return ref.read(fileMentionProvider).filter(m.$2).take(8).toList();
  }

  /// Ctrl/⌘+V：剪贴板里是图就当附件收下，是文字就照常粘。
  ///
  /// # ⚠️ 文字优先，而且这不是随手定的
  ///
  /// 从网页复制一段带插图的内容，剪贴板里**两样都有**。十次里九次用户要的
  /// 是文字 —— 反过来的话，他会得到一张莫名其妙的图而正文没了，
  /// 还得手动删掉那张图再重新复制。
  ///
  /// # 为什么要自己插入文字，而不是「放行让 TextField 自己粘」
  ///
  /// 快捷键一旦被 `Shortcuts` 接住就不会再往下走，没有「我不处理」这个
  /// 返回值可用（判据是异步的 —— 得先读一次剪贴板才知道里面是什么，
  /// 而 `Action.isEnabled` 是同步的）。所以文字这条路要手动补上替换选区
  /// 与移动光标，与原生粘贴逐字对齐。
  Future<void> _paste() async {
    if (!widget.enabled) return;

    final text = await Pasteboard.text;
    if (!mounted) return;
    // 先只读文字：有文字就不必再去问图片（那一步在某些平台上会弹权限）
    if (choosePaste(text: text, hasImage: false) == PasteChoice.text) {
      final sel = _controller.selection;
      final old = _controller.text;
      // 选区可能是「无效」的（从没聚焦过）—— 那时按插到末尾处理，
      // 直接用 sel.start 会是 -1 并抛 RangeError
      final start = sel.start < 0 ? old.length : sel.start;
      final end = sel.end < 0 ? old.length : sel.end;
      _controller.value = TextEditingValue(
        text: old.replaceRange(start, end, text!),
        selection: TextSelection.collapsed(offset: start + text.length),
      );
      return;
    }

    final bytes = await Pasteboard.image;
    if (!mounted) return;
    if (bytes == null ||
        choosePaste(text: null, hasImage: bytes.isNotEmpty) !=
            PasteChoice.image) {
      return;
    }
    // 粘一张图**就是开口** —— 白纸在这里兑现成一条真会话
    final id = widget.sessionId ?? widget.ensureSession();
    await ref
        .read(attachmentQueueProvider.notifier)
        .addBytes(
          id,
          // 剪贴板里的图没有文件名。带上时间戳而不是固定名字：同一轮里粘
          // 两张的话，两个「粘贴的图片.png」在托盘里分不出谁是谁。
          // ⚠️ 一律 .png —— `Pasteboard.image` 在各平台上给的都是 PNG 字节，
          // 而真正的判据在服务端（登记 blob 时从字节头嗅探，不信这个名字）
          filename: '粘贴的图片-${DateTime.now().millisecondsSinceEpoch}.png',
          bytes: bytes,
          mime: 'image/png',
        );
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
      // ⚠️ 不再要求 `sessionId != null`：白纸上拖图进来是合法的，
      // 会话在那一刻兑现（见 [MessageComposer.ensureSession]）
      enable: widget.enabled,
      onDragEntered: (_) => setState(() => _dragging = true),
      onDragExited: (_) => setState(() => _dragging = false),
      onDragDone: (detail) {
        setState(() => _dragging = false);
        if (detail.files.isEmpty) return;
        ref
            .read(attachmentQueueProvider.notifier)
            .addDropped(sessionId ?? widget.ensureSession(), detail.files);
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
            // 与消息列同宽。两条边落在同一条竖线上，是「这些东西属于一起」
            // 最省力的表达 —— 差 4px 反而比差 100px 更难看
            constraints: const BoxConstraints(maxWidth: kConversationWidth),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              mainAxisSize: MainAxisSize.min,
              children: [
                if (sessionId != null)
                  PendingAttachmentTray(sessionId: sessionId),
                if (_mention != null)
                  _MentionList(
                    index: ref.watch(fileMentionProvider),
                    options: _candidates(),
                    selected: _mentionAt,
                    onPick: _insertMention,
                  ),
                Container(
                  decoration: BoxDecoration(
                    color: _dragging
                        ? scheme.primary.withValues(alpha: 0.06)
                        : scheme.surfaceContainerHigh,
                    // 输入框是圆角五档里最圆的一档（18，比对话框 14 还圆）：
                    // 它要读出胶囊感 —— 这一档只给它，别处不用
                    borderRadius: BorderRadius.circular(
                      CortexTokens.radiusInput,
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
                          // 白纸上也能选文件 —— 会话在选完那一刻兑现
                          onPressed: widget.enabled
                              ? () => ref
                                    .read(attachmentQueueProvider.notifier)
                                    .pickAndUpload(
                                      sessionId ?? widget.ensureSession(),
                                    )
                              : null,
                          iconSize: 19,
                          tooltip: '添加附件（也可以直接拖进来或粘贴）',
                          icon: const Icon(Icons.attach_file_rounded),
                        ),
                      ),
                      Expanded(
                        child: Shortcuts(
                          shortcuts: const {
                            SingleActivator(LogicalKeyboardKey.enter):
                                _SendIntent(),
                            SingleActivator(LogicalKeyboardKey.arrowDown):
                                _MentionMoveIntent(1),
                            SingleActivator(LogicalKeyboardKey.arrowUp):
                                _MentionMoveIntent(-1),
                            SingleActivator(LogicalKeyboardKey.escape):
                                _MentionDismissIntent(),
                            // 粘贴要自己接管，因为 TextField 的原生粘贴只认
                            // 文本 —— 剪贴板里那张图它当作「什么都没有」。
                            // 两个键位都列上：Windows/Linux 是 Ctrl，macOS 是 ⌘
                            SingleActivator(
                              LogicalKeyboardKey.keyV,
                              control: true,
                            ): _PasteIntent(),
                            SingleActivator(
                              LogicalKeyboardKey.keyV,
                              meta: true,
                            ): _PasteIntent(),
                          },
                          child: Actions(
                            actions: {
                              _SendIntent: CallbackAction<_SendIntent>(
                                onInvoke: (_) {
                                  // 引用列表开着时，Enter 是「选中这一条」，
                                  // 不是「发出去」—— 直接发的话，用户刚打了
                                  // 一半的路径会被当成正文送走
                                  final options = _candidates();
                                  if (_mention != null && options.isNotEmpty) {
                                    _insertMention(
                                      options[_mentionAt % options.length],
                                    );
                                    return null;
                                  }
                                  _submit();
                                  return null;
                                },
                              ),
                              _PasteIntent: CallbackAction<_PasteIntent>(
                                onInvoke: (_) {
                                  unawaited(_paste());
                                  return null;
                                },
                              ),
                              _MentionMoveIntent:
                                  CallbackAction<_MentionMoveIntent>(
                                    onInvoke: (intent) {
                                      final options = _candidates();
                                      if (_mention == null || options.isEmpty) {
                                        return null;
                                      }
                                      setState(() {
                                        _mentionAt =
                                            (_mentionAt + intent.delta) %
                                            options.length;
                                        if (_mentionAt < 0) {
                                          _mentionAt += options.length;
                                        }
                                      });
                                      return null;
                                    },
                                  ),
                              _MentionDismissIntent:
                                  CallbackAction<_MentionDismissIntent>(
                                    onInvoke: (_) {
                                      _setMention(null);
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
                // LayoutBuilder 在这里拿得到真实宽度（Column 给的是有界
                // 约束）；放进 Row 里就拿不到了 —— Row 给非弹性子项的是
                // 无界宽
                LayoutBuilder(
                  builder: (context, cons) {
                    // 四颗 chip 摊开约 460px。窄过它就收：从前是固定
                    // flex，放不下直接溢出报黄条
                    final narrow = cons.maxWidth < 460;
                    return Row(
                      children: [
                        // 「这轮的文件去哪儿」与「谁来把关」是同一类东西：
                        // 发出去**之前**要定的事。所以它们贴着输入框，而
                        // 不是挂在顶栏 —— 顶栏那一排是应用级的显示开关
                        _BeforeSendChips(narrow: narrow),
                        // 居中于**剩下的那段**，而不是整行。
                        //
                        // 这里原本在右边挂一个透明的等宽占位，好让那句话
                        // 相对整行严格居中。两个 chip 之后那个代价变实了：
                        // 占位是一份**真的 widget**，读屏会念两遍。
                        // 只在**还没开始聊**的那一屏说 —— 常驻的无动作
                        // 文案会训练人忽略那一整片区域
                        if (widget.centred && !narrow)
                          Expanded(
                            child: Text(
                              'Cortex 会把这轮对话归档，之后还能搜得到。',
                              textAlign: TextAlign.center,
                              style: theme.textTheme.labelSmall,
                            ),
                          ),
                      ],
                    );
                  },
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
/// `@` 之后那张候选文件列表。
///
/// # 为什么画在输入框**上方**，而不是用 Overlay 悬浮
///
/// 悬浮层要自己算位置、跟着窗口缩放走、还要在滚动时重定位。而这里的输入框
/// 本来就贴在底部，正上方那块地方是空的 —— 直接占用它，没有任何定位逻辑
/// 可以出错。
class _MentionList extends StatelessWidget {
  const _MentionList({
    required this.index,
    required this.options,
    required this.selected,
    required this.onPick,
  });

  final MentionIndex index;
  final List<String> options;
  final int selected;
  final void Function(String) onPick;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    // 没绑工作区就整个不出现。一个引用了文件的问题发给一个没有文件工具的
    // agent，得到的回答会一本正经地跑偏 —— 而用户看不出哪里错了
    if (!index.available) return const SizedBox.shrink();

    final Widget body;
    if (index.loading) {
      body = const Padding(
        padding: EdgeInsets.symmetric(horizontal: 12, vertical: 10),
        child: Row(
          children: [
            SizedBox(
              width: 12,
              height: 12,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
            SizedBox(width: 8),
            Text('正在看工作区里有什么…'),
          ],
        ),
      );
    } else if (index.error != null) {
      body = Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
        child: Text(
          index.error!,
          style: theme.textTheme.bodySmall?.copyWith(color: scheme.error),
        ),
      );
    } else if (options.isEmpty) {
      body = Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
        child: Text(
          '没有匹配的文件',
          style: theme.textTheme.bodySmall?.copyWith(
            color: scheme.onSurfaceVariant,
          ),
        ),
      );
    } else {
      body = Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (var i = 0; i < options.length; i++)
            _MentionRow(
              path: options[i],
              highlighted: i == selected,
              onTap: () => onPick(options[i]),
            ),
          // 清单不完整这件事要说。不说的话，用户打了个名字搜不到，
          // 会以为那个文件不存在
          if (index.truncated)
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 4, 12, 8),
              child: Text(
                '工作区文件很多，这里只收了前 $kMentionScanLimit 个',
                style: theme.textTheme.labelSmall?.copyWith(
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ),
        ],
      );
    }

    return Container(
      margin: const EdgeInsets.only(bottom: 6),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        border: Border.all(color: scheme.outlineVariant),
      ),
      clipBehavior: Clip.antiAlias,
      child: body,
    );
  }
}

class _MentionRow extends StatelessWidget {
  const _MentionRow({
    required this.path,
    required this.highlighted,
    required this.onTap,
  });

  final String path;
  final bool highlighted;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final slash = path.lastIndexOf('/');
    final dir = slash < 0 ? '' : path.substring(0, slash + 1);
    final name = slash < 0 ? path : path.substring(slash + 1);

    return InkWell(
      onTap: onTap,
      child: Container(
        width: double.infinity,
        // **中性**（规范第九节）：这是键盘光标停在哪一条，是位置不是动作。
        //
        // 与下面那个拖放高亮（`_dragging` 用 primary）分得开是有意的 ——
        // 那个表达的是「松手就会发生一件事」，这个只表达「我现在在这一行」
        color: highlighted ? theme.cortex.sidebarAccent : null,
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 7),
        child: Row(
          children: [
            Icon(
              Icons.insert_drive_file_outlined,
              size: 14,
              color: scheme.onSurfaceVariant,
            ),
            const SizedBox(width: 8),
            Flexible(
              child: Text.rich(
                TextSpan(
                  children: [
                    // 目录淡一点、文件名正常：一屏八条路径全同色的话，
                    // 眼睛得逐条从右往左找文件名
                    if (dir.isNotEmpty)
                      TextSpan(
                        text: dir,
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    TextSpan(text: name, style: theme.textTheme.bodyMedium),
                  ],
                ),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _BeforeSendChips extends StatelessWidget {
  const _BeforeSendChips({required this.narrow});

  /// 窄到摊不开四颗时收成「目录 + 权限档 + …」。
  ///
  /// # 为什么留下的是这两颗
  ///
  /// 判据是**改错的代价**，不是使用频率：工作区决定文件落在哪、权限档
  /// 决定命令有没有人把关 —— 这两个选错了是真损失，而且难在事后发现。
  /// 模型与智能体改错了只影响这一轮答得好不好，收进「…」里翻一下够用。
  final bool narrow;

  @override
  Widget build(BuildContext context) {
    if (!narrow) {
      return const Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          // 工作区在前：它决定文件落在哪，是两者里更容易选错、
          // 也更难在事后发现选错了的那一个
          WorkspaceChip(),
          SizedBox(width: 6),
          PermissionModeChip(),
          SizedBox(width: 6),
          // 模型排最后：它是三者里唯一**改错了也不会造成损失**的一个
          // （只影响这一轮答得好不好、花多少钱），而前两个改错了是
          // 文件写错地方、命令没人把关
          ModelChip(),
          // 智能体单独排在最后，且大多数时候**根本不出现**（没建过智能体、
          // 或这条对话用的是默认人设）—— 它不是逐轮的决定，与左边三个不同类
          SizedBox(width: 6),
          AssistantChip(),
        ],
      );
    }
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        const WorkspaceChip(),
        const SizedBox(width: 6),
        const PermissionModeChip(),
        const SizedBox(width: 2),
        // 收进来的 chip **原样**放进菜单，不是重画一份菜单项：
        // 它们各自的弹层、锁定态、不可用时自隐藏的逻辑都在 chip 里，
        // 抄成菜单项等于那些行为都要再维护一份
        MenuAnchor(
          menuChildren: const [
            Padding(
              padding: EdgeInsets.fromLTRB(10, 8, 10, 4),
              child: ModelChip(),
            ),
            Padding(
              padding: EdgeInsets.fromLTRB(10, 4, 10, 8),
              child: AssistantChip(),
            ),
          ],
          builder: (context, controller, _) => IconButton(
            onPressed: () =>
                controller.isOpen ? controller.close() : controller.open(),
            iconSize: 16,
            visualDensity: VisualDensity.compact,
            tooltip: '模型与智能体',
            icon: const Icon(Icons.more_horiz_rounded),
          ),
        ),
      ],
    );
  }
}

/// 在候选列表里上下移动。
class _MentionMoveIntent extends Intent {
  const _MentionMoveIntent(this.delta);
  final int delta;
}

/// 关掉候选列表。
class _MentionDismissIntent extends Intent {
  const _MentionDismissIntent();
}

class _SendIntent extends Intent {
  const _SendIntent();
}

/// Ctrl/⌘+V。见 [`_MessageComposerState._paste`]。
class _PasteIntent extends Intent {
  const _PasteIntent();
}

/// 一次粘贴该收下什么。
enum PasteChoice { text, image, nothing }

/// 剪贴板里两样都有时取哪一样。
///
/// 拆成纯函数是为了可测：真正的调用点在一段异步 IO 里
/// （`Pasteboard.text` / `Pasteboard.image` 都走 platform channel），
/// 从测试里够不着 —— 与 `app_providers.dart` 的 `shouldForwardUnauthorized`
/// 同一个理由。
///
/// ⚠️ **文字优先。** 从网页复制一段带插图的内容，剪贴板里两样都有，
/// 而十次里九次用户要的是文字。反过来的话他会得到一张莫名其妙的图、
/// 正文没了，还得手动删掉再重新复制一次。
///
/// 局限说清楚：这里只钉住判据，钉不住「接线有没有接对」。
@visibleForTesting
PasteChoice choosePaste({required String? text, required bool hasImage}) {
  if (text != null && text.isNotEmpty) return PasteChoice.text;
  return hasImage ? PasteChoice.image : PasteChoice.nothing;
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
