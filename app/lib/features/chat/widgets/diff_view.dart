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
      child: Scrollbar(
        child: SingleChildScrollView(
          padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
          child: SingleChildScrollView(
            // 横向也要能滚：diff 里长行很常见，折行会把 `+`/`-` 的对齐
            // 打散，而那个对齐正是一眼看出增删的依据
            scrollDirection: Axis.horizontal,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisSize: MainAxisSize.min,
              children: [
                for (final line in lines)
                  if (line.isNotEmpty || lines.length == 1)
                    Text(
                      line,
                      style: TextStyle(
                        fontFamily: 'JetBrains Mono',
                        fontFamilyFallback: CortexTheme.monoFallback,
                        fontSize: 12,
                        height: 1.35,
                        color: switch (_kindOf(line)) {
                          _LineKind.added => added,
                          _LineKind.removed => removed,
                          // 跳过标记与截断说明：说的是「这里省略了东西」，
                          // 不是内容本身，所以压暗
                          _LineKind.meta => scheme.onSurfaceVariant,
                          _LineKind.context => scheme.onSurface,
                        },
                      ),
                    ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

enum _LineKind { added, removed, context, meta }

_LineKind _kindOf(String line) {
  if (line.startsWith('+')) return _LineKind.added;
  if (line.startsWith('-')) return _LineKind.removed;
  if (line.startsWith('@@') || line.startsWith('…') || line.startsWith('（')) {
    return _LineKind.meta;
  }
  return _LineKind.context;
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
    switch (_kindOf(line)) {
      case _LineKind.added:
        a++;
      case _LineKind.removed:
        r++;
      case _LineKind.context || _LineKind.meta:
        break;
    }
  }
  return (added: a, removed: r);
}
