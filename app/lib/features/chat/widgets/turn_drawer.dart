import 'diff_view.dart';
import 'package:flutter/material.dart';

import '../../../core/ansi.dart';
import '../../../models/tool_call.dart';

/// 一轮里 agent **做了什么** —— 工具调用与它们改动的内容。
///
/// * **这一轮还在跑时**，工具行直接铺开。一个跑了两秒的 `read_file` 正是
///   「怎么半天没动静」的答案，藏进一次点击后面等于白做。
/// * **结束后**收进一行细字：审计轨迹要在每个回答上都拿得到，
///   但不该和回答本身抢注意力。
///
/// ## 它曾经还渲染「本轮用到的记忆」
///
/// 那一半随记忆界面一起去了 Cormex —— 客户端这一侧不再有任何记忆界面。
/// **改名是这次改动的一部分**：它此前叫 `MemoryDrawer`，剥掉记忆之后那个
/// 名字会指着一件它不再做的事，下一个来读的人会去找记忆在哪儿。
class TurnDrawer extends StatefulWidget {
  const TurnDrawer({
    super.key,
    this.toolCalls = const [],
    this.streaming = false,
    this.initiallyExpanded = false,
  });

  final List<ToolCall> toolCalls;

  /// 这一轮还在飞 —— 工具活动摊开，不收起来。
  final bool streaming;

  final bool initiallyExpanded;

  @override
  State<TurnDrawer> createState() => _TurnDrawerState();
}

class _TurnDrawerState extends State<TurnDrawer> {
  late bool _expanded = widget.initiallyExpanded;

  @override
  Widget build(BuildContext context) {
    if (widget.toolCalls.isEmpty) return const SizedBox.shrink();

    if (widget.streaming) {
      return Padding(
        padding: const EdgeInsets.only(top: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [for (final call in widget.toolCalls) _ToolRow(call: call)],
        ),
      );
    }

    return Padding(
      padding: const EdgeInsets.only(top: 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _Toggle(
            expanded: _expanded,
            label: '本轮工具调用 · ${widget.toolCalls.length} 次',
            onTap: () => setState(() => _expanded = !_expanded),
          ),
          AnimatedSize(
            duration: const Duration(milliseconds: 160),
            curve: Curves.easeOutCubic,
            alignment: Alignment.topLeft,
            child: _expanded
                ? _Panel(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        for (final call in widget.toolCalls)
                          _ToolRow(call: call),
                      ],
                    ),
                  )
                : const SizedBox.shrink(),
          ),
        ],
      ),
    );
  }
}

class _Panel extends StatelessWidget {
  const _Panel({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.only(top: 8),
      padding: const EdgeInsets.fromLTRB(12, 12, 12, 6),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(11),
        border: Border.all(color: scheme.secondary.withValues(alpha: 0.28)),
        color: scheme.secondary.withValues(alpha: 0.04),
      ),
      child: child,
    );
  }
}

class _Toggle extends StatelessWidget {
  const _Toggle({
    required this.expanded,
    required this.label,
    required this.onTap,
  });

  final bool expanded;
  final String label;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(7),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 4),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.psychology_outlined, size: 14, color: scheme.secondary),
            const SizedBox(width: 6),
            Text(
              label,
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.secondary,
                fontWeight: FontWeight.w600,
              ),
            ),
            const SizedBox(width: 2),
            AnimatedRotation(
              turns: expanded ? 0.5 : 0,
              duration: const Duration(milliseconds: 160),
              child: Icon(
                Icons.expand_more_rounded,
                size: 15,
                color: scheme.secondary,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// One invocation — call and result on a single line.
/// 一次工具调用。有 diff 的话可以展开看这次改了什么。
///
/// 默认**收着**：一轮里可能有五六次写入，全部摊开会把这一段变成一片
/// 看不完的绿红，而抽屉本来是「扫一眼这轮干了什么」用的。
class _ToolRow extends StatefulWidget {
  const _ToolRow({required this.call});

  final ToolCall call;

  @override
  State<_ToolRow> createState() => _ToolRowState();
}

class _ToolRowState extends State<_ToolRow> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final call = widget.call;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final labelStyle = theme.textTheme.labelSmall ?? const TextStyle();

    final Color icon;
    if (call.failed) {
      icon = scheme.error;
    } else if (call.pending) {
      icon = scheme.secondary;
    } else {
      icon = scheme.onSurfaceVariant;
    }

    final path = call.path;

    final row = Padding(
      padding: const EdgeInsets.only(left: 7, bottom: 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(top: 1),
            child: Icon(
              call.failed
                  ? Icons.error_outline_rounded
                  // A file tool gets a file icon: at a glance the row says
                  // "this touched your disk" rather than "something ran".
                  : call.touchesFiles
                  ? Icons.description_outlined
                  : Icons.terminal_rounded,
              size: 13,
              color: icon,
            ),
          ),
          const SizedBox(width: 7),
          Expanded(
            child: RichText(
              text: TextSpan(
                style: labelStyle,
                children: [
                  TextSpan(
                    text: call.name,
                    style: labelStyle.copyWith(
                      fontFamily: 'monospace',
                      fontWeight: FontWeight.w600,
                      color: scheme.onSurface,
                    ),
                  ),
                  // The file a file-tool touched gets the emphasis the rest of
                  // the arguments do not. "write_file 改了哪个文件" is the one
                  // question a reader always has about these rows, and leaving
                  // it buried mid-string next to a truncated `content=` makes
                  // it effectively invisible.
                  if (path != null)
                    TextSpan(
                      text: '  $path',
                      style: labelStyle.copyWith(
                        fontFamily: 'monospace',
                        color: scheme.secondary,
                      ),
                    )
                  else if (call.arguments != null)
                    TextSpan(text: '  ${call.arguments}'),
                  // 工具输出走 ANSI 解析：`shell` 跑的是真终端命令，
                  // cargo / npm / git 一律带色。不解析的话那些序列**原样**
                  // 进界面，用户看到的是 `[32m通过[0m` 而不是一个绿色的
                  // 「通过」。失败时不解析 —— 那一行整条要是错误色，
                  // 让命令自己的配色去覆盖它只会把「这条挂了」冲淡
                  if (call.result != null)
                    if (call.failed)
                      TextSpan(
                        text: '  · ${stripAnsi(call.result!)}',
                        style: labelStyle.copyWith(color: scheme.error),
                      )
                    else ...[
                      TextSpan(
                        text: '  · ',
                        style: labelStyle.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                      ...parseAnsi(
                        call.result!,
                        base: labelStyle.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    ],
                ],
              ),
            ),
          ),
          // 有改动才给展开箭头 —— 一个点下去什么都不展开的箭头，
          // 比没有箭头更让人困惑
          if (call.diff != null)
            InkResponse(
              onTap: () => setState(() => _open = !_open),
              radius: 12,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 4),
                child: Icon(
                  _open
                      ? Icons.keyboard_arrow_down_rounded
                      : Icons.keyboard_arrow_right_rounded,
                  size: 15,
                  color: scheme.onSurfaceVariant,
                ),
              ),
            ),
          if (call.pending) ...[
            const SizedBox(width: 8),
            Padding(
              padding: const EdgeInsets.only(top: 2),
              child: SizedBox(
                width: 9,
                height: 9,
                child: CircularProgressIndicator(
                  strokeWidth: 1.4,
                  color: scheme.secondary,
                ),
              ),
            ),
          ],
        ],
      ),
    );

    if (!_open || call.diff == null) return row;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        row,
        Padding(
          padding: const EdgeInsets.only(left: 27, bottom: 8, right: 4),
          child: DiffView(call.diff!),
        ),
      ],
    );
  }
}
