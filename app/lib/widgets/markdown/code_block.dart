import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../core/theme.dart';
import 'highlight_registry.dart';
import '../../core/motion.dart';

/// A fenced code block with a header (language + copy) and horizontally
/// scrollable, syntax-highlighted body.
///
/// Streaming-aware: [closed] is false while the closing fence has not arrived
/// yet, which the header shows as a subtle pulse instead of letting the block
/// silently look finished.
class CodeBlock extends StatefulWidget {
  const CodeBlock({
    super.key,
    required this.code,
    required this.language,
    this.closed = true,
  });

  final String code;
  final String? language;
  final bool closed;

  @override
  State<CodeBlock> createState() => _CodeBlockState();
}

class _CodeBlockState extends State<CodeBlock> {
  late final ScrollController _horizontal = ScrollController();

  // Highlighting is pure but not free, and during streaming `code` changes on
  // every delta while theme/brightness do not. Caching on the (code, language,
  // brightness) triple keeps us to exactly one highlight pass per new byte
  // rather than one per rebuild (rebuilds also fire on hover, scroll, etc).
  String? _cachedCode;
  String? _cachedLanguage;
  Brightness? _cachedBrightness;
  TextSpan? _cachedSpan;

  bool _copied = false;

  @override
  void dispose() {
    _horizontal.dispose();
    super.dispose();
  }

  TextSpan _span(TextStyle baseStyle, Brightness brightness) {
    if (_cachedCode == widget.code &&
        _cachedLanguage == widget.language &&
        _cachedBrightness == brightness &&
        _cachedSpan != null) {
      return _cachedSpan!;
    }
    final highlighted = HighlightRegistry.highlight(
      code: widget.code,
      language: widget.language,
      baseStyle: baseStyle,
      brightness: brightness,
    );
    final span = highlighted ?? TextSpan(text: widget.code, style: baseStyle);
    _cachedCode = widget.code;
    _cachedLanguage = widget.language;
    _cachedBrightness = brightness;
    _cachedSpan = span;
    return span;
  }

  Future<void> _copy() async {
    await Clipboard.setData(ClipboardData(text: widget.code));
    if (!mounted) return;
    setState(() => _copied = true);
    await Future<void>.delayed(const Duration(milliseconds: 1600));
    if (mounted) setState(() => _copied = false);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final brightness = theme.brightness;

    // Root style from the highlight theme so the block background matches the
    // token colours rather than fighting them.
    final hlTheme = HighlightRegistry.themeFor(brightness);
    final rootBg =
        hlTheme['root']?.backgroundColor ??
        (brightness == Brightness.dark
            ? const Color(0xFF15171A)
            : const Color(0xFFF7F8FA));
    final rootFg = hlTheme['root']?.color ?? scheme.onSurface;

    final baseStyle = TextStyle(
      fontFamily: 'monospace',
      fontFamilyFallback: CortexTheme.monoFallback,
      fontSize: 12.8,
      height: 1.55,
      color: rootFg,
    );

    final label = HighlightRegistry.labelFor(widget.language);

    return Container(
      margin: const EdgeInsets.symmetric(vertical: 10),
      decoration: BoxDecoration(
        color: rootBg,
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        border: Border.all(color: scheme.outlineVariant),
      ),
      clipBehavior: Clip.antiAlias,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          _Header(
            label: label,
            streaming: !widget.closed,
            copied: _copied,
            onCopy: _copy,
          ),
          Scrollbar(
            controller: _horizontal,
            thumbVisibility: false,
            child: SingleChildScrollView(
              controller: _horizontal,
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.fromLTRB(14, 12, 14, 14),
              child: SelectionArea(
                child: Text.rich(
                  _span(baseStyle, brightness),
                  softWrap: false,
                  textAlign: TextAlign.left,
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _Header extends StatelessWidget {
  const _Header({
    required this.label,
    required this.streaming,
    required this.copied,
    required this.onCopy,
  });

  final String label;
  final bool streaming;
  final bool copied;
  final VoidCallback onCopy;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      height: 34,
      padding: const EdgeInsets.only(left: 14, right: 6),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: scheme.outlineVariant)),
      ),
      child: Row(
        children: [
          Text(
            label,
            style: TextStyle(
              fontFamily: 'monospace',
              fontFamilyFallback: CortexTheme.monoFallback,
              fontSize: 11.5,
              letterSpacing: 0.3,
              color: scheme.onSurfaceVariant,
            ),
          ),
          if (streaming) ...[
            const SizedBox(width: 8),
            _StreamingDot(color: scheme.primary),
          ],
          const Spacer(),
          IconButton(
            onPressed: onCopy,
            iconSize: 15,
            visualDensity: VisualDensity.compact,
            padding: const EdgeInsets.all(6),
            constraints: const BoxConstraints(),
            tooltip: copied ? '已复制' : '复制代码',
            icon: Icon(
              copied ? Icons.check_rounded : Icons.copy_rounded,
              color: copied ? scheme.secondary : scheme.onSurfaceVariant,
            ),
          ),
        ],
      ),
    );
  }
}

/// Breathing dot shown while a fence is still open.
class _StreamingDot extends StatefulWidget {
  const _StreamingDot({required this.color});
  final Color color;

  @override
  State<_StreamingDot> createState() => _StreamingDotState();
}

class _StreamingDotState extends State<_StreamingDot>
    with SingleTickerProviderStateMixin {
  late final AnimationController _c = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 900),
  );

  // 「减少动效」开着时停在完全不透明：这个点表示「还在输出」，
  // 让它消失等于把唯一的进度证据一起关掉。见 core/motion.dart
  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    syncLoop(_c, context, reverse: true);
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return FadeTransition(
      opacity: _c.drive(Tween(begin: 0.25, end: 1.0)),
      child: Container(
        width: 6,
        height: 6,
        decoration: BoxDecoration(color: widget.color, shape: BoxShape.circle),
      ),
    );
  }
}
