import 'package:flutter/material.dart';
import 'package:gpt_markdown/custom_widgets/markdown_config.dart';
import 'package:gpt_markdown/gpt_markdown.dart';

import '../../design_system/design_system.dart';
import 'code_block.dart';

/// Markdown renderer for assistant output.
///
/// ## Why `gpt_markdown`
///
/// The candidates were `flutter_markdown`, `markdown_widget` and
/// `gpt_markdown`. Streaming is the deciding axis here, because every delta
/// hands the renderer a document that is *syntactically incomplete* — an
/// unterminated fence, half a table row, a dangling `**`.
///
/// * `flutter_markdown` is discontinued upstream (superseded by the community
///   `flutter_markdown_plus` fork). It also parses via `package:markdown` into
///   a full AST each build and renders an unterminated ``` fence as literal
///   backticks until the closing fence arrives — the block visibly *flips*
///   from paragraph to code mid-stream. Ruled out on both counts.
/// * `markdown_widget` has good code-block support and a TOC, but is built
///   around whole-document layout with heading anchors; the same
///   incomplete-fence flip applies.
/// * `gpt_markdown` is written specifically for LLM token streams. Its
///   `CodeBlockBuilder` receives a `closed` flag, i.e. it *models* the
///   half-arrived fence as a first-class state instead of mis-parsing it. That
///   is exactly the requirement — the block renders as code from the first
///   token and just keeps growing, so there is no reflow flash.
///
/// ## Why `re_highlight` for the code
///
/// `gpt_markdown` deliberately ships no highlighter, so the pairing is ours to
/// choose. `flutter_highlight` wraps the older `highlight` package (last
/// meaningful release years ago, ~36 grammars, and its `Node` tree is rebuilt
/// per frame). `re_highlight` is a current port of highlight.js 11 maintained
/// by the Reqable team, covers ~190 grammars including Rust/Dart/SQL, exposes
/// a `TextSpanRenderer` that produces an `InlineSpan` directly (no widget tree
/// per token), and lets us register grammars individually to keep the web
/// bundle small. See `highlight_registry.dart`.
class CortexMarkdown extends StatelessWidget {
  const CortexMarkdown(this.data, {super.key, this.onLinkTap});

  final String data;
  final void Function(String url)? onLinkTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    // 正文跟随全局 15px 字阶，并稍微放宽行距以适合桌面端长回答。
    final bodyStyle = CortexType.body(
      context,
    ).copyWith(color: scheme.onSurface, height: 1.72);

    return GptMarkdownTheme(
      gptThemeData: GptMarkdownThemeData(
        brightness: theme.brightness,
        // 链接用 accentInk 而不是 primary：主色是为按钮底调的，
        // 直接当正文里的文字用对比度不够（见 CortexTokens.accentInk 的注）
        linkColor: theme.cortex.accentInk,
        linkHoverColor: theme.cortex.accentInk,
        hrLineColor: scheme.outlineVariant,
        hrLineThickness: 1,
        highlightColor: scheme.surfaceContainerHigh,
        // 标题阶：h1 21 → h3 16 → h6 14，**每一档都比正文只大一档**。
        // 对话里的标题是分节，不是文章章节 —— 差三档会让一段回答读起来
        // 像三个页面拼在一起
        h1: bodyStyle.copyWith(
          fontSize: 20,
          fontWeight: FontWeight.w600,
          letterSpacing: 0,
          height: 1.4,
        ),
        h2: bodyStyle.copyWith(
          fontSize: 17,
          fontWeight: FontWeight.w600,
          letterSpacing: 0,
          height: 1.45,
        ),
        h3: bodyStyle.copyWith(
          fontSize: 16,
          fontWeight: FontWeight.w600,
          letterSpacing: 0,
          height: 1.5,
        ),
        h4: bodyStyle.copyWith(fontSize: 15, fontWeight: FontWeight.w600),
        h5: bodyStyle.copyWith(fontSize: 14.5, fontWeight: FontWeight.w600),
        h6: bodyStyle.copyWith(
          fontSize: 14,
          fontWeight: FontWeight.w600,
          color: scheme.onSurfaceVariant,
        ),
      ),
      child: GptMarkdown(
        data,
        style: bodyStyle,
        onLinkTap: (url, _) => onLinkTap?.call(url),
        codeBuilder: (context, name, code, closed) =>
            CodeBlock(code: code, language: name, closed: closed),
        highlightBuilder: (context, text, style) =>
            _InlineCode(text: text, style: style),
        tableBuilder: (context, rows, textStyle, config) =>
            _MarkdownTable(rows: rows, textStyle: textStyle, config: config),
      ),
    );
  }
}

/// Inline `code` — a subtle chip rather than a coloured word, so it stays
/// readable inside a dense Chinese paragraph.
class _InlineCode extends StatelessWidget {
  const _InlineCode({required this.text, required this.style});

  final String text;
  final TextStyle style;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    return Container(
      margin: const EdgeInsets.symmetric(horizontal: 1.5),
      padding: const EdgeInsets.symmetric(
        horizontal: CortexSpace.xs,
        vertical: 1.5,
      ),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(CortexRadius.row),
        border: CortexStroke.all(scheme.outlineVariant),
      ),
      child: Text(
        text,
        style: style.copyWith(
          fontFamily: 'JetBrains Mono',
          fontFamilyFallback: CortexTheme.monoFallback,
          fontSize: (style.fontSize ?? 14) * 0.9,
          // 行内代码用**中性**的深一档，不用彩色：一段中文里嵌三个
          // 彩色的词，读起来像三处链接。字重与底色已经把它分出来了
          color: theme.cortex.accentInk,
        ),
      ),
    );
  }
}

/// Tables get a horizontal scroller and zebra striping.
///
/// The package default lays every column out at intrinsic width inside the
/// message column, so a 4-column comparison table overflows on a narrow pane.
class _MarkdownTable extends StatefulWidget {
  const _MarkdownTable({
    required this.rows,
    required this.textStyle,
    required this.config,
  });

  final List<CustomTableRow> rows;
  final TextStyle textStyle;
  final GptMarkdownConfig config;

  @override
  State<_MarkdownTable> createState() => _MarkdownTableState();
}

class _MarkdownTableState extends State<_MarkdownTable> {
  late final ScrollController _controller = ScrollController();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final rows = widget.rows;
    if (rows.isEmpty) return const SizedBox.shrink();

    final columnCount = rows
        .map((r) => r.fields.length)
        .fold<int>(0, (a, b) => a > b ? a : b);
    if (columnCount == 0) return const SizedBox.shrink();

    return Container(
      margin: const EdgeInsets.symmetric(vertical: CortexSpace.md),
      decoration: BoxDecoration(
        border: CortexStroke.all(scheme.outlineVariant),
        borderRadius: BorderRadius.circular(CortexRadius.card),
      ),
      clipBehavior: Clip.antiAlias,
      child: Scrollbar(
        controller: _controller,
        child: SingleChildScrollView(
          controller: _controller,
          scrollDirection: Axis.horizontal,
          child: ConstrainedBox(
            constraints: const BoxConstraints(minWidth: 320),
            child: Table(
              defaultColumnWidth: const IntrinsicColumnWidth(flex: 1),
              defaultVerticalAlignment: TableCellVerticalAlignment.middle,
              border: TableBorder.symmetric(
                inside: BorderSide(color: scheme.outlineVariant),
              ),
              children: [
                for (var r = 0; r < rows.length; r++)
                  TableRow(
                    decoration: BoxDecoration(
                      color: r == 0
                          ? scheme.surfaceContainerHigh
                          : (r.isEven
                                ? scheme.surfaceContainerLow
                                : Colors.transparent),
                    ),
                    children: [
                      for (var c = 0; c < columnCount; c++)
                        Padding(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 14,
                            vertical: 9,
                          ),
                          child: MdWidget(
                            context,
                            c < rows[r].fields.length
                                ? rows[r].fields[c].data
                                : '',
                            false,
                            config: widget.config.copyWith(
                              style: widget.textStyle.copyWith(
                                fontWeight: r == 0
                                    ? FontWeight.w600
                                    : FontWeight.w400,
                                fontSize:
                                    (widget.textStyle.fontSize ?? 15) - 0.8,
                              ),
                            ),
                          ),
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
