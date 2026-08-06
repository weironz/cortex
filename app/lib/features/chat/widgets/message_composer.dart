import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

/// Input box.
///
/// Enter sends, Shift+Enter inserts a newline — implemented with a
/// [Shortcuts]/[Actions] override rather than by inspecting raw key events, so
/// IME composition (essential for Chinese input) is not interrupted: while a
/// candidate window is open the framework consumes Enter itself and our action
/// never fires.
class MessageComposer extends StatefulWidget {
  const MessageComposer({
    super.key,
    required this.onSend,
    required this.onStop,
    required this.streaming,
    this.enabled = true,
  });

  final ValueChanged<String> onSend;
  final VoidCallback onStop;
  final bool streaming;
  final bool enabled;

  @override
  State<MessageComposer> createState() => _MessageComposerState();
}

class _MessageComposerState extends State<MessageComposer> {
  final TextEditingController _controller = TextEditingController();
  final FocusNode _focusNode = FocusNode();
  bool _hasText = false;

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
    final text = _controller.text.trim();
    if (text.isEmpty) return;
    _controller.clear();
    widget.onSend(text);
    _focusNode.requestFocus();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Container(
      decoration: BoxDecoration(
        color: scheme.surface,
        border: Border(top: BorderSide(color: scheme.outlineVariant)),
      ),
      padding: const EdgeInsets.fromLTRB(20, 14, 20, 16),
      child: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 820),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            mainAxisSize: MainAxisSize.min,
            children: [
              Container(
                decoration: BoxDecoration(
                  color: scheme.surfaceContainerHigh,
                  borderRadius: BorderRadius.circular(14),
                  border: Border.all(
                    color: _focusNode.hasFocus
                        ? scheme.primary
                        : scheme.outlineVariant,
                  ),
                ),
                padding: const EdgeInsets.fromLTRB(14, 4, 6, 4),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.end,
                  children: [
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
                            minLines: 1,
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
                                  ? '问点什么…（Enter 发送，Shift+Enter 换行）'
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
                              onPressed: _hasText && widget.enabled
                                  ? _submit
                                  : null,
                              tooltip: '发送',
                              icon: Icons.arrow_upward_rounded,
                              background: _hasText && widget.enabled
                                  ? scheme.primary
                                  : scheme.surfaceContainerHighest,
                              foreground: _hasText && widget.enabled
                                  ? scheme.onPrimary
                                  : scheme.onSurfaceVariant,
                            ),
                    ),
                  ],
                ),
              ),
              const SizedBox(height: 7),
              Text(
                'Cortex 会把这轮对话归档，并从中抽取可追溯的记忆。',
                textAlign: TextAlign.center,
                style: theme.textTheme.labelSmall,
              ),
            ],
          ),
        ),
      ),
    );
  }
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
