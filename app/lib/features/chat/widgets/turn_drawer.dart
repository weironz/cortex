import 'diff_view.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/ansi.dart';
import '../../../core/motion.dart';
import '../../../models/tool_call.dart';
import '../../../design_system/design_system.dart';
import '../../../state/app_providers.dart';
import '../../../state/chat_controller.dart';
import '../../workspace/right_rail.dart';

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

  /// 这一轮还在飞时，前面那些已完成的调用摊没摊开。
  bool _liveExpanded = false;

  /// 跑着的时候，末尾始终留几条完成的可见。
  ///
  /// 全折起来的话界面上只剩一个「已完成 N 次」在跳数字，看不出它在往哪个
  /// 方向走；而全摊开就是用户报的那个「直接平铺出来了」—— 一次十几二十
  /// 次调用把回答挤出屏幕。留 2 条是「刚做了什么」与「别刷屏」的折中。
  static const int _tailVisible = 2;

  /// 少于这个数就不折 —— 折起 1、2 行换来一行「已完成 N 次」，
  /// 净收益是零，还多一次点击。
  static const int _foldThreshold = 3;

  @override
  Widget build(BuildContext context) {
    if (widget.toolCalls.isEmpty) return const SizedBox.shrink();

    if (widget.streaming) {
      // ── 跑着的时候：完成的收起来，正在跑的留着 ──
      //
      // 从前这里是**整个摊平**。一轮二十次调用的话，二十行工具把回答本身
      // 挤出了屏幕，而那二十行里用户真正在看的只有最后一两条 ——
      // 2026-08-30 实报：「直接平铺出来了」。
      //
      // 折的是**前面那些已完成的**：正在跑的那条必须一直可见（它是唯一
      // 说明「此刻在等什么」的东西），末尾几条完成的也留着（只剩一个跳
      // 数字的计数器，看不出它在往哪个方向走）。
      final done = widget.toolCalls.where((c) => !c.pending).toList();
      final live = widget.toolCalls.where((c) => c.pending).toList();
      final foldCount = done.length - _tailVisible;
      final folded = foldCount >= _foldThreshold;
      final hidden = folded
          ? done.take(foldCount).toList()
          : const <ToolCall>[];
      final shown = folded ? done.skip(foldCount).toList() : done;

      return Padding(
        padding: const EdgeInsets.only(top: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (folded) ...[
              _Toggle(
                expanded: _liveExpanded,
                label: '已完成 ${hidden.length} 次',
                onTap: () => setState(() => _liveExpanded = !_liveExpanded),
              ),
              if (_liveExpanded)
                for (final call in hidden) ToolRow(call: call),
            ],
            for (final call in shown) ToolRow(call: call),
            for (final call in live) ToolRow(call: call),
          ],
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
            // 展开工具调用列表时下面的内容整块位移 —— 位移正是前庭敏感
            // 最难受的一类，比循环的三个点更该跟着这个开关走
            duration: CortexMotion.resolve(context, CortexMotion.base),
            curve: CortexMotion.enter,
            alignment: Alignment.topLeft,
            child: _expanded
                ? _Panel(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        for (final call in widget.toolCalls)
                          ToolRow(call: call),
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
      margin: const EdgeInsets.only(top: CortexSpace.md),
      padding: const EdgeInsets.fromLTRB(
        CortexSpace.lg,
        CortexSpace.lg,
        CortexSpace.lg,
        CortexSpace.sm,
      ),
      decoration: BoxDecoration(
        borderRadius: BorderRadius.circular(CortexRadius.card),
        // 中性容器而不是 secondary 淡染：这块面板是审计轨迹，不表达任何
        // 状态，彩色边框会让它看起来像一条常驻的提醒（反馈色只在传达
        // 状态时用，不当装饰）
        border: CortexStroke.all(scheme.outlineVariant),
        color: scheme.surfaceContainer,
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

    return CortexHover(
      onTap: onTap,
      builder: (context, hot) => AnimatedContainer(
        duration: CortexMotion.resolve(context, CortexMotion.instant),
        padding: const EdgeInsets.symmetric(
          horizontal: CortexSpace.md,
          vertical: CortexSpace.xs,
        ),
        decoration: BoxDecoration(
          // 折叠入口画成状态胶囊：面板色垫底 + 发丝线，与工具行的
          // 胶囊同族，一眼读得出「这是一组过程记录的开关」。
          // 悬停时边框提亮 —— 这是它「能点」的唯一视觉信号
          color: scheme.surfaceContainer,
          borderRadius: BorderRadius.circular(CortexRadius.pill),
          border: CortexStroke.all(
            hot
                ? scheme.onSurface.withValues(alpha: 0.18)
                : scheme.outlineVariant,
          ),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            // terminal 而不是 psychology：后者是「本轮用到的记忆」时代的
            // 遗留，现在这行摊开的只有工具调用。整行中性灰 —— 折叠行是
            // 入口不是状态，彩色在这里只是装饰；w600 留着，靠字重而不是
            // 颜色把它与正文分开
            Icon(
              Icons.terminal_rounded,
              size: 13,
              color: scheme.onSurfaceVariant,
            ),
            const SizedBox(width: CortexSpace.sm),
            Text(
              label,
              style: theme.textTheme.labelSmall?.copyWith(
                color: scheme.onSurfaceVariant,
                fontWeight: FontWeight.w600,
              ),
            ),
            const SizedBox(width: CortexSpace.xxs),
            AnimatedRotation(
              turns: expanded ? 0.5 : 0,
              duration: CortexMotion.resolve(context, CortexMotion.base),
              curve: CortexMotion.enter,
              child: Icon(
                Icons.expand_more_rounded,
                size: 14,
                color: scheme.onSurfaceVariant,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// 胶囊里那颗「此刻在跑」的呼吸灯。
///
/// # 为什么呼吸而不是转圈
///
/// 转环（CircularProgressIndicator）是**加载**的语言 —— 它许诺「有进度，
/// 只是我不知道多少」，而一次工具调用是离散的：发出、等待、返回。
/// 呼吸的明暗脉冲表达的是「这件事活着、正在等回音」，与 Claude /
/// DeepSeek 思考面板那颗脉冲灯是同一种语言。
///
/// 动效的开关走 [syncLoop]：用户选了「减少动效」时停在全亮的静止点，
/// 而不是消失（消失 = 「这个状态不存在了」）。
class _PulseDot extends StatefulWidget {
  const _PulseDot({super.key, required this.color});

  final Color color;

  @override
  State<_PulseDot> createState() => _PulseDotState();
}

class _PulseDotState extends State<_PulseDot>
    with SingleTickerProviderStateMixin {
  late final AnimationController _c = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 900),
  );

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
    return AnimatedBuilder(
      animation: _c,
      builder: (context, _) => Container(
        width: 7,
        height: 7,
        decoration: BoxDecoration(
          shape: BoxShape.circle,
          color: widget.color.withValues(alpha: 0.30 + 0.70 * _c.value),
          // 一圈同色淡晕：呼吸的「光」，而不是第二个点
          boxShadow: [
            BoxShadow(
              color: widget.color.withValues(alpha: 0.35 * _c.value),
              blurRadius: 4,
              spreadRadius: 0.5,
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
/// 一次工具调用的**那一行**，不带任何标题或外框。
///
/// 公开是为了**骨架内联**用（见 `AssistantBlock` 里那段）：块把工具画进正文
/// 中间时，要的就是这一行本身。裹一层 [TurnDrawer] 的话会带出它那行
/// 「本轮工具调用 · N 次」的标题 —— 一轮二十次调用就是二十行「· 1 次」，
/// 而「本轮」这个词用在单独一次调用上本身也是错的。
class ToolRow extends StatefulWidget {
  const ToolRow({super.key, required this.call});

  final ToolCall call;

  @override
  State<ToolRow> createState() => _ToolRowState();
}

class _ToolRowState extends State<ToolRow> {
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
      // primary 而不是 secondary：「在跑」在会话行上是蓝点（info/primary
      // 同族），同一个语义在这里换个颜色，读的人要多背一条对照
      icon = scheme.primary;
    } else {
      // 第三级：图标在这一行里只说明「这是一次工具调用」，
      // 而那件事整行的样式已经说过一遍了
      icon = theme.cortex.foregroundTertiary;
    }

    final path = call.path;

    // 子 agent 干的活往里缩一格。
    //
    // 不做成「分组折叠块」是有意的：四路并行时事件是**交错**到达的，
    // 分组要么等全部收工再画（那期间界面上什么都没有，而用户正等着看
    // 它在干嘛），要么每来一条就重排一次列表（行会跳）。缩进不需要重排，
    // 而层级关系照样一眼看得出。
    final indent = call.subagent == null ? 7.0 : 22.0;

    final row = Container(
      margin: EdgeInsets.only(left: indent, bottom: CortexSpace.sm),
      padding: const EdgeInsets.symmetric(
        horizontal: CortexSpace.md,
        vertical: CortexSpace.xs,
      ),
      decoration: BoxDecoration(
        // ── 状态胶囊 ──
        //
        // 面板色垫底 + 发丝线 + 行档圆角：一行一次调用就是一个轻量胶囊，
        // 与子 agent 的缩进（见上面 indent）叠用 —— 子任务的胶囊整体
        // 内缩，层级靠位置表达，不靠更深的颜色（深色下颜色越深越
        // 「陷进去」，正是要避免的方向）
        color: scheme.surfaceContainer,
        borderRadius: BorderRadius.circular(CortexRadius.row),
        border: CortexStroke.all(scheme.outlineVariant),
      ),
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
                  // 哪个子 agent 干的。**要写出来，不能只靠缩进** ——
                  // 四路并行时缩进一样深，光看缩进分不出这一行属于谁，
                  // 而「A 的结果显示在 B 名下」正是这个功能最容易犯的错
                  if (call.subagent case final tag?)
                    TextSpan(
                      text: '子 ${tag.index} · ',
                      style: labelStyle.copyWith(
                        color: theme.cortex.foregroundTertiary,
                      ),
                    ),
                  TextSpan(
                    text: call.name,
                    style: labelStyle.copyWith(
                      fontFamily: 'JetBrains Mono',
                      fontFamilyFallback: CortexTheme.monoFallback,
                      fontWeight: FontWeight.w600,
                      // ⚠️ 曾经是 onSurface —— **与回答正文同一档**。
                      // 工具调用是过程，回答才是结论，两者一样黑的时候
                      // 眼睛分不出该先读哪个。整块退到第二级，
                      // 行内的层次改由**字重**承担（见下面参数与结果那两档）
                      color: scheme.onSurfaceVariant,
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
                        fontFamily: 'JetBrains Mono',
                        fontFamilyFallback: CortexTheme.monoFallback,
                        // ⚠️ 曾经是 accentInk（品牌色）。路径确实是这行里
                        // 要被认出来的对象，但**用彩色去做这件事**会让每一条
                        // 工具行都挂着一块品牌色，整段过程于是比结论还显眼。
                        // 色彩只表达动作或含义（见 theme.dart 第一节）——
                        // 「这是个路径」两者都不是。改由字重区分：
                        // 路径 w500，参数与结果不加重
                        color: scheme.onSurfaceVariant,
                        fontWeight: FontWeight.w500,
                      ),
                    )
                  else if (call.arguments != null)
                    // 参数是这一行里最不重要的一截：它常是被截断的，
                    // 读它不如读结果。压到第三级
                    TextSpan(
                      text: '  ${call.arguments}',
                      style: labelStyle.copyWith(
                        color: theme.cortex.foregroundTertiary,
                      ),
                    ),
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
                          color: theme.cortex.foregroundTertiary,
                        ),
                      ),
                      ...parseAnsi(
                        call.result!,
                        base: labelStyle.copyWith(
                          color: theme.cortex.foregroundTertiary,
                        ),
                        // 深色主题要亮档色板 —— 不传的话 cargo 的红字
                        // 在深底上只有 3.5:1，编译报错恰好最读不清
                        brightness: theme.brightness,
                      ),
                    ],
                ],
              ),
            ),
          ),
          // 「这次改了多少」贴在行尾。
          //
          // 它是看到一行 `write_file src/x.rs` 时的第一个问题，而回答它
          // 此前要先点开箭头、再目测一屏绿红。**这是整行里唯一保留彩色的
          // 东西** —— 增删是含义不是装饰，而且这两个数字正是用来决定
          // 「要不要点开」的。GitHub 与 Claude Code 都把它放在文件名旁边
          if (call.diff case final diff?) ...[
            const SizedBox(width: 8),
            Padding(
              padding: const EdgeInsets.only(top: 1),
              child: DiffStat(diff),
            ),
          ],
          // 有东西可展开才给箭头 —— 一个点下去什么都不展开的箭头，
          // 比没有箭头更让人困惑。可展开的三种：改动（diff）、失败的完整
          // 输出（行内被压成一行，真正的报错常在后半段）、以及这次调用的
          // **真实输出**。
          //
          // ⚠️ 第三种只有**这一轮在看的时候**才有：`output` 随事件到达，
          // 而 `episode_tool_calls` 里没有这一列。打开一个旧会话时它是 null，
          // 于是那些行就没有箭头 —— 这正是「有东西可展开才给箭头」要的，
          // 不是漏了
          if (call.diff != null ||
              call.output != null ||
              (call.failed && call.result != null))
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
          // 运行状态点：pending 是呼吸灯（它前面那截转环是「加载」的语言，
          // 见 _PulseDot）；成功与失败不靠点 —— 失败已有红色图标与红色
          // 输出，成功什么都不挂（常态不需要状态标记，挂了只会训练人
          // 忽略它）
          if (call.pending) ...[
            const SizedBox(width: 8),
            Padding(
              padding: const EdgeInsets.only(top: 3),
              // key 是给测试的锚点：pending 迹象有过一次改头换面
              // （转环 → 呼吸灯），钉实现细节（`find.byType`）的测试
              // 跟着碎了一回 —— 钉 key 钉的是「有 pending 迹象」这个意图
              child: _PulseDot(
                key: const ValueKey('turn:tool-pulse'),
                color: scheme.primary,
              ),
            ),
          ],
        ],
      ),
    );

    final failedDetail = call.failed && call.result != null;
    // 失败时优先给报错（那是用户要找的东西），否则给这次调用的真实输出
    final body = failedDetail ? stripAnsi(call.result!) : call.output;
    if (!_open || (call.diff == null && body == null)) return row;

    // 失败展开：完整 stderr（等宽、可选中）+「让它自己修」。
    // 修不是玄学 —— 就是把失败的命令与输出发回去当下一条消息，
    // 模型看得到全文，比用户手动复述「刚才那个报错」强得多
    if (call.diff == null) {
      final text = body!;
      return Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          row,
          Padding(
            padding: const EdgeInsets.only(
              left: 27,
              bottom: CortexSpace.md,
              right: CortexSpace.xs,
            ),
            child: Container(
              width: double.infinity,
              decoration: BoxDecoration(
                color: scheme.surfaceContainerHigh,
                borderRadius: BorderRadius.circular(CortexRadius.row),
                border: CortexStroke.all(scheme.outlineVariant),
              ),
              padding: const EdgeInsets.all(CortexSpace.md),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  SelectionArea(
                    child: Text(
                      text,
                      style: CortexType.mono(
                        context,
                        size: 12,
                        color: scheme.onSurface,
                      ).copyWith(height: 1.5),
                    ),
                  ),
                  // 「让它自己修」只在**失败**时给。成功那次给一个「修」的
                  // 按钮，是在暗示它出了问题
                  if (failedDetail)
                    Align(
                      alignment: Alignment.centerRight,
                      child: Consumer(
                        builder: (context, ref, _) => TextButton.icon(
                          style: TextButton.styleFrom(
                            visualDensity: VisualDensity.compact,
                            textStyle: const TextStyle(fontSize: 11),
                          ),
                          onPressed: () => ref
                              .read(chatControllerProvider.notifier)
                              .send(
                                '刚才 ${call.name} 失败了：\n\n'
                                '```\n${stripAnsi(call.result!)}\n```\n\n'
                                '请自己修复这个问题，然后重试。',
                              ),
                          icon: const Icon(Icons.build_outlined, size: 13),
                          label: const Text('让它自己修'),
                        ),
                      ),
                    ),
                ],
              ),
            ),
          ),
        ],
      );
    }

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        row,
        Padding(
          padding: const EdgeInsets.only(left: 27, bottom: 8, right: 4),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              // 长 diff 在气泡里只能看个开头（DiffView 有高度上限）。
              // 「在右栏打开」去的是「本轮改动」页签 —— 那里有这个文件
              // 的全部几次改动，且与对话并排、能一直开着对照读
              Align(
                alignment: Alignment.centerRight,
                child: Consumer(
                  builder: (context, ref, _) => TextButton.icon(
                    style: TextButton.styleFrom(
                      visualDensity: VisualDensity.compact,
                      textStyle: const TextStyle(fontSize: 11),
                    ),
                    onPressed: () {
                      ref.read(railTabProvider.notifier).go(RailTab.changes);
                      ref
                          .read(layoutProvider.notifier)
                          .showRight(RightPanel.files);
                      // 窄屏上右栏放不下内联，从底部抽上来；
                      // 宽屏 showRight 已经让内联栏出现了
                      if (MediaQuery.sizeOf(context).width <
                          kRailInlineBreakpoint) {
                        showRightRailSheet(context);
                      }
                    },
                    icon: const Icon(Icons.open_in_new_rounded, size: 13),
                    label: const Text('在右栏打开'),
                  ),
                ),
              ),
              DiffView(call.diff!),
            ],
          ),
        ),
      ],
    );
  }
}
