import 'package:flutter/material.dart';

import '../../../core/theme.dart';

/// 一份统一 diff，按行着色。
///
/// # 为什么值得单独有一个组件
///
/// 它出现在三个地方 —— 确认框（批准之前）、工具行（批准之后）、会话改动
/// 汇总（回头看）。三处的问题是同一个：**这次到底改了什么**。同一个问题
/// 画三种样子，人每换一个地方就要重新认一遍颜色。
///
/// # 为什么不复用 Markdown 的代码块
///
/// 那个是按**语言**做语法高亮的，而 diff 要按**行首字符**着色 ——
/// 一行 `-  let x = 1;` 里重要的不是 `let` 是关键字，是这行被删了。
/// 把 diff 丢进语法高亮器，会得到一屏五颜六色但看不出增删的东西。
///
/// # 为什么增删行既染字又垫底色
///
/// 只染字的话，一行中文改动里 `+` 后面跟的全是全角字，得**逐字**扫颜色
/// 才认得出增删；一条通底的色带在余光里就分得开。
///
/// 但**两条线索都要留**：只垫底色的话，高对比模式与红绿色盲下那点淡色
/// 是没有的，而那时行首的 `+`/`-` 与文字颜色是仅剩的依据。
class DiffView extends StatelessWidget {
  const DiffView(this.diff, {super.key, this.maxHeight = 260});

  final String diff;

  /// 超过就滚动。不给上限的话，一份四百行的 diff 会把确认框顶出屏幕，
  /// 而「允许 / 拒绝」两个按钮正在它下面。
  final double maxHeight;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final lines = diff.split('\n');

    // 绿加红减是这件事的通用语言（git、GitHub、每一个 code review 工具）。
    // 换一套配色只会让人多认一遍。
    // 取主题里按深浅配好对的语义色，不写死单值：一个色值不可能同时在
    // 深浅两底上够对比 —— 之前那对写死的在浅色下只有 3.1:1
    final added = theme.cortex.success;
    final removed = scheme.error;

    return Container(
      constraints: BoxConstraints(maxHeight: maxHeight),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHighest.withValues(alpha: 0.35),
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: LayoutBuilder(
        builder: (context, box) => Scrollbar(
          child: SingleChildScrollView(
            padding: const EdgeInsets.symmetric(vertical: 6),
            child: SingleChildScrollView(
              // 横向也要能滚：diff 里长行很常见，折行会把 `+`/`-` 的对齐
              // 打散，而那个对齐正是一眼看出增删的依据
              scrollDirection: Axis.horizontal,
              child: ConstrainedBox(
                // 色带要通到右边缘，而横向滚动里每行只有内容那么宽 ——
                // 不撑开的话，短的增行是一小截色块、长的是一长条，
                // 看起来像在表达长度，而它不表达任何东西。
                //
                // ⚠️ 取的是 [LayoutBuilder] 给的**本容器**宽度，不是
                // `MediaQuery` 的窗口宽度：这个组件也出现在确认框那种
                // 窄容器里，按窗口宽撑的话那里会凭空多出一条横向滚动条。
                constraints: BoxConstraints(minWidth: box.maxWidth),
                // stretch 要求宽度有界，而横向滚动里它是无穷的 ——
                // 少了这一层是 `BoxConstraints forces an infinite width`。
                // 代价是量一遍每行的固有宽度，而 diff 在上游已经截到
                // 400 行，量得起
                child: IntrinsicWidth(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      for (final line in lines)
                        if (line.isNotEmpty || lines.length == 1)
                          _DiffLine(
                            line: line,
                            kind: diffLineKind(line),
                            added: added,
                            removed: removed,
                            meta: scheme.onSurfaceVariant,
                            context_: scheme.onSurface,
                          ),
                    ],
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _DiffLine extends StatelessWidget {
  const _DiffLine({
    required this.line,
    required this.kind,
    required this.added,
    required this.removed,
    required this.meta,
    required this.context_,
  });

  final String line;
  final DiffLineKind kind;
  final Color added;
  final Color removed;
  final Color meta;
  final Color context_;

  @override
  Widget build(BuildContext context) {
    final fg = switch (kind) {
      DiffLineKind.added => added,
      DiffLineKind.removed => removed,
      // 跳过标记与截断说明：说的是「这里省略了东西」，
      // 不是内容本身，所以压暗
      DiffLineKind.meta => meta,
      DiffLineKind.context => context_,
    };
    // 很淡的一层。浓了的话一屏全是色块，又回到「看不出重点」——
    // 它要做的只是让眼睛在**不读字**的情况下分出增删两片
    final bg = switch (kind) {
      DiffLineKind.added => added.withValues(alpha: 0.11),
      DiffLineKind.removed => removed.withValues(alpha: 0.11),
      DiffLineKind.meta || DiffLineKind.context => null,
    };

    return Container(
      color: bg,
      padding: const EdgeInsets.symmetric(horizontal: 8),
      child: Text(
        line,
        style: TextStyle(
          fontFamily: 'JetBrains Mono',
          fontFamilyFallback: CortexTheme.monoFallback,
          fontSize: 12,
          height: 1.35,
          color: fg,
        ),
      ),
    );
  }
}

/// 「+12 −4」那一小块。
///
/// # 为什么它值得在工具行上占一块地方
///
/// 「这次改动大不大」是看到一行 `write_file src/x.rs` 时的第一个问题，
/// 而回答它此前要先点开箭头、再目测一屏绿红。GitHub、Claude Code、
/// 每一个 code review 工具都把这两个数字放在文件名旁边，不是巧合。
///
/// ⚠️ 用 `−`（U+2212）不是 `-`：等宽字体里减号比连字符宽，与 `+` 对得齐。
class DiffStat extends StatelessWidget {
  const DiffStat(this.diff, {super.key});

  final String diff;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final stat = countChanges(diff);
    final style = (theme.textTheme.labelSmall ?? const TextStyle()).copyWith(
      fontFamily: 'JetBrains Mono',
      fontFamilyFallback: CortexTheme.monoFallback,
      fontWeight: FontWeight.w600,
    );
    return RichText(
      text: TextSpan(
        children: [
          TextSpan(
            text: '+${stat.added}',
            style: style.copyWith(color: theme.cortex.success),
          ),
          const TextSpan(text: ' '),
          TextSpan(
            text: '−${stat.removed}',
            style: style.copyWith(color: theme.colorScheme.error),
          ),
        ],
      ),
    );
  }
}

/// 一行 diff 是哪一类。
///
/// 公开出来是因为**测试与统计都要按同一套判据走** —— 各判各的时候，
/// 汇总面板上的数字与屏幕上的颜色会对不上，而那个数字正是人用来判断
/// 「这次改动大不大」的东西。
enum DiffLineKind { added, removed, context, meta }

DiffLineKind diffLineKind(String line) {
  if (line.startsWith('+')) return DiffLineKind.added;
  if (line.startsWith('-')) return DiffLineKind.removed;
  if (line.startsWith('@@') || line.startsWith('…') || line.startsWith('（')) {
    return DiffLineKind.meta;
  }
  return DiffLineKind.context;
}

/// 供测试与汇总面板复用：这份 diff 里增删各多少行。
///
/// 只数**内容行**：`@@` 与截断说明不是改动。数错了不会有任何报错，
/// 只是汇总面板上的数字对不上 —— 而那个数字正是人用来判断
/// 「这次改动大不大」的东西。
({int added, int removed}) countChanges(String diff) {
  var a = 0;
  var r = 0;
  for (final line in diff.split('\n')) {
    switch (diffLineKind(line)) {
      case DiffLineKind.added:
        a++;
      case DiffLineKind.removed:
        r++;
      case DiffLineKind.context || DiffLineKind.meta:
        break;
    }
  }
  return (added: a, removed: r);
}
