import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../core/theme.dart';
import 'highlight_registry.dart';
import '../../core/motion.dart';

/// 收起时露出多高。
///
/// 按**像素**给而不是按行数截字符串：截字符串会把语法高亮在半途切断
/// （一个没闭合的字符串会把后面几行全染成字面量色），而且复制按钮
/// 拿到的还得是全文 —— 高度上限与「哪些字」两件事分开做才都不出错。
///
/// 240px ÷ 12.8px × 1.55 行高 ≈ **12 行**。这个数与下面那个阈值有关系，
/// 见那里。
const _collapsedHeight = 240.0;

/// 超过多少行就默认收起。
///
/// ⚠️ **必须明显大于 [_collapsedHeight] 露得出的行数（≈12）。** 两者贴太近
/// 的话，一个刚过线的块收起来只藏住三五行 —— 省下的屏幕还不够那一次点击的
/// 成本，而用户看到的是「一个点了没什么用的按钮」。
/// 第一版取 16，实地一看正是这个样子。
///
/// 24 行 ≈ 一个不算短的函数：到这个长度为止整块摊着仍读得完，
/// 再长就一定值得收。
const kCodeCollapseLines = 24;

/// A fenced code block with a header (language + copy) and horizontally
/// scrollable, syntax-highlighted body.
///
/// Streaming-aware: [closed] is false while the closing fence has not arrived
/// yet, which the header shows as a subtle pulse instead of letting the block
/// silently look finished.
///
/// # 长块默认收起
///
/// 一个三百行的代码块会把整条对话顶出屏幕，而它上下那两句**结论**才是
/// 用户要读的。超过 [kCodeCollapseLines] 行就压到 [_collapsedHeight] 高，
/// 底下压一层渐隐加一行「展开全部 · 共 N 行」——「还有多少没看到」
/// 必须说出来，一个没有数字的「展开」按钮无法让人判断该不该点。
///
/// ⚠️ **正在流式输出的块不收**（[closed] 为假）：那时用户正看着它长，
/// 而收起会把刚吐出来的几行藏起来。而且一旦收过一次，`closed` 变真时
/// 突然塌下去会把下面的内容整块上移 —— 位移正是最难受的一类动效。
/// 所以状态里记着「这一块是在眼前长出来的」，它不再自动收。
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

  /// 用户自己点过展开/收起了吗。点过就一直听他的 —— 一个每次重建
  /// 都弹回默认值的开关，会让人以为自己没点到。
  bool? _userExpanded;

  /// 这一块是在眼前流式长出来的吗。见类文档。
  bool _everStreamed = false;

  /// 这一次到底展不展开。
  ///
  /// 顺序有意如此：**用户的选择最大**，其次是「它在眼前长过」，
  /// 最后才是行数。反过来的话，用户收起一个正在长的块之后
  /// 下一个 token 就把它顶开了。
  bool get _expanded {
    if (_userExpanded case final choice?) return choice;
    if (_everStreamed || !widget.closed) return true;
    return _lineCount <= kCodeCollapseLines;
  }

  int get _lineCount => const LineSplitter().convert(widget.code).length;

  bool get _collapsible => _lineCount > kCodeCollapseLines;

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
    if (!widget.closed) _everStreamed = true;

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
      fontFamily: 'JetBrains Mono',
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
          // 收起时给一个高度上限而不是截断字符串：截字符串会把
          // 语法高亮在半途切断（一个没闭合的字符串把后面全染成字面量色），
          // 而且复制按钮拿到的还得是全文 —— 两件事分开做才都不出错
          ClipRect(
            child: Align(
              alignment: Alignment.topLeft,
              heightFactor: 1,
              child: ConstrainedBox(
                constraints: BoxConstraints(
                  maxHeight: _expanded ? double.infinity : _collapsedHeight,
                ),
                child: Scrollbar(
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
              ),
            ),
          ),
          if (_collapsible)
            _Expander(
              expanded: _expanded,
              lines: _lineCount,
              background: rootBg,
              onTap: () => setState(() => _userExpanded = !_expanded),
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
              fontFamily: 'JetBrains Mono',
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

/// 展开 / 收起那一条。
///
/// # 为什么要写「共 N 行」
///
/// 一个光写「展开」的按钮无法让人判断该不该点：底下是十行还是三百行，
/// 决定完全不同。数字在这里不是装饰，是这次点击的**代价**。
class _Expander extends StatelessWidget {
  const _Expander({
    required this.expanded,
    required this.lines,
    required this.background,
    required this.onTap,
  });

  final bool expanded;
  final int lines;

  /// 代码块自己的底色。渐隐要从**它**过渡到透明，取主题的 surface
  /// 会在高亮主题与界面主题不同源时露出一条错色的边。
  final Color background;

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return Stack(
      children: [
        // 收起时在这一条上方压一层渐隐 —— 「下面还有」这件事要在
        // 看见按钮**之前**就被余光接收到，否则一刀切齐的边缘读起来
        // 像代码本来就到此为止
        if (!expanded)
          Positioned(
            top: -28,
            left: 0,
            right: 0,
            height: 28,
            child: IgnorePointer(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  gradient: LinearGradient(
                    begin: Alignment.topCenter,
                    end: Alignment.bottomCenter,
                    colors: [background.withValues(alpha: 0), background],
                  ),
                ),
              ),
            ),
          ),
        InkWell(
          onTap: onTap,
          child: Container(
            width: double.infinity,
            padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 7),
            decoration: BoxDecoration(
              border: Border(top: BorderSide(color: scheme.outlineVariant)),
            ),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  expanded
                      ? Icons.keyboard_arrow_up_rounded
                      : Icons.keyboard_arrow_down_rounded,
                  size: 15,
                  color: scheme.onSurfaceVariant,
                ),
                const SizedBox(width: 5),
                Text(
                  expanded ? '收起' : '展开全部 · 共 $lines 行',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: scheme.onSurfaceVariant,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}
