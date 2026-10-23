import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../design_system/design_system.dart';
import '../../../models/turn_usage.dart';
import '../../../state/chat_controller.dart';
import '../../../state/chat_state.dart';
import '../../../state/model_controller.dart';

/// 输入框底下那条**用量状态栏**。
///
/// 显示这一轮真的花掉了什么：生成速度、输入输出 token、缓存命中、
/// 上下文填充率、工具步数。
///
/// # 一条规矩：每个数字要么来自供应商的报告，要么不画
///
/// 这不是洁癖。状态栏最容易被填上「看起来合理」的数 —— 按字数猜 token、
/// 按模型的窗口上限编一个剩余量 —— 而用户会拿它判断「还能聊多久」。
/// 猜错的表现是**没有任何症状**：数字每天都在，只是不对。
/// 所以供应商没报的项整项不出现（见 [TurnUsage]），而不是画一个 0。
///
/// 唯一一处估算的是**轮内**速度（`~` 前缀标着），因为它要跟着字一起动，
/// 而 token 数要到这一段结束才到。段一结束就换成真值。
class UsageStatusBar extends ConsumerStatefulWidget {
  const UsageStatusBar({
    super.key,
    required this.sessionId,
    this.availableWidth,
  });

  /// 哪条会话。`null` = 白纸（还没开口），整条不出现。
  final String? sessionId;

  /// 整行宽。用来在**画之前**决定还摆得下几组数字，而不是画完再等黄条。
  ///
  /// # 为什么从外面喂进来
  ///
  /// 行宽只有 [LayoutBuilder] 拿得到，而 Row 的**子项**拿不到（无界约束）。
  /// 从外面喂进来，这里就能按「chips 摊开后剩下多少」收那一档，而不是
  /// 写死一个窗口宽度 —— 输入框两侧的留白是随形态变的。
  final double? availableWidth;

  @override
  ConsumerState<UsageStatusBar> createState() => _UsageStatusBarState();
}

class _UsageStatusBarState extends ConsumerState<UsageStatusBar> {
  Timer? _tick;

  @override
  void dispose() {
    _tick?.cancel();
    super.dispose();
  }

  /// 生成中每秒重画一次。
  ///
  /// 流在吐字时状态本来就每次 flush 变一次，不需要这个；但**停下来的时候
  /// 不会变** —— 模型去跑一条两分钟的命令时，那个速度会冻在最后一个值上，
  /// 看起来像「它还在飞快地生成」。每秒重画一次，它就会掉下去。
  void _keepTicking({required bool streaming}) {
    if (streaming) {
      _tick ??= Timer.periodic(const Duration(seconds: 1), (_) {
        if (mounted) setState(() {});
      });
    } else if (_tick != null) {
      _tick!.cancel();
      _tick = null;
    }
  }

  @override
  Widget build(BuildContext context) {
    final sessionId = widget.sessionId;
    if (sessionId == null) {
      // 白纸状态：这一屏没有会话，可**上一屏那个 Timer 可能还在跑**。
      // 不在这儿停掉的话，每秒一次的 setState 会一直空转到用户再选中
      // 某条会话（或者整个撰写框被销毁）为止 —— 而屏幕上什么都没有
      _keepTicking(streaming: false);
      return const SizedBox.shrink();
    }

    final turn = ref.watch(
      chatControllerProvider.select((s) => s.streamingTurns[sessionId]),
    );
    final last = ref.watch(
      chatControllerProvider.select((s) => s.turnUsages[sessionId]),
    );
    final catalog = ref.watch(modelCatalogProvider).value;

    _keepTicking(streaming: turn != null);

    // 这一轮还没报出任何一条用量时，先摆着上一轮的数字（暗一档）——
    // 一发送就整条消失、几秒后又冒出来，比一直有数更晃眼
    final known = turn == null || turn.usage.isEmpty ? last : turn.usage;
    // 一条数都还没有（本次运行的第一次对话），但这一轮**已经在跑了**：
    // 摆一个走秒的表。空白五秒再冒出一行数字，读起来像「刚才是不是卡了」
    if (known == null && turn == null) return const SizedBox.shrink();
    final usage = known ?? const TurnUsage();

    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    // 正在跑 = 这一轮已经报过数了。没报过的话那条是上一轮的，读数是旧的
    final live = turn != null && !turn.usage.isEmpty;
    final liveSpeed = _liveSpeed(turn, usage);
    // 有一个**活的**数字（真值、估算，或者「这一轮在跑」那个秒表）就提亮
    // 一档；否则摆着的是上一轮的，压暗 —— 同一个位置，两种含义要分得出
    final color = live || liveSpeed != null || (known == null && turn != null)
        ? scheme.onSurfaceVariant
        : theme.cortex.foregroundTertiary;

    final window = usage.lastModel == null
        ? null
        : catalog?.byId(usage.lastModel!)?.context;
    final percent = contextPercent(usage.lastInput, window);

    final items = <Widget>[];

    void add(Widget w) {
      if (items.isNotEmpty) {
        items.add(
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: CortexSpace.xs),
            child: Text('·', style: CortexType.caption(context)),
          ),
        );
      }
      items.add(w);
    }

    // 在跑的时候数**这一轮**已经调了几次工具（比 `usage.steps` 早 ——
    // 那个要等收尾时控制器才补上），跑完之后读收尾时存下的那份
    final steps = turn == null ? usage.steps : turn.toolCalls.length;
    // chips 之外还剩多宽（_BeforeSendChips 摊开实测约 810 —— 见
    // message_composer 那头的注释）。这是**先量后画**的一档：宽裕给全部
    // 五组，不宽裕只留速度与上下文 —— 那两组正是这个功能存在的理由，
    // 其余是「顺路告诉你」。Row 不截断溢出，画多了是黄条
    final spare = (widget.availableWidth ?? double.infinity) - 810;
    final showAll = spare >= 100;
    if (showAll && steps > 0) {
      add(
        _stat(context, '$steps 步', color: color, tooltip: '这一轮调用了 $steps 次工具。'),
      );
    }
    if (liveSpeed != null) {
      add(
        _stat(
          context,
          '~${liveSpeed.round()} tok/s',
          color: color,
          tooltip:
              '按已生成的字数估算 —— 供应商要到这一段结束才报 token 数，'
              '届时这个「~」会换成真值。',
        ),
      );
    } else if (usage.tokensPerSecond != null) {
      final tps = usage.tokensPerSecond!;
      add(
        _stat(
          context,
          '${tps.round()} tok/s',
          color: color,
          tooltip:
              '${live ? '这一轮' : '上一轮'}的生成速度：'
              '${usage.timedOutput} 个输出 token ÷ '
              '${(usage.timedMs / 1000).toStringAsFixed(1)} 秒'
              '（只算模型在产出的那段时间，含思考与工具调用）。',
        ),
      );
    } else if (turn != null) {
      // 量不出速度（这一段只吐了几滴字，或者压根还没报数），
      // 那就如实说已经跑了多久
      // 系统时钟可以往回跳（对时、夏令时）。负数秒会画成 `-3s`，
      // 那不是「跑了负三秒」，是我们拿墙钟当秒表用了 —— 下限兜住
      final secs = DateTime.now()
          .difference(turn.startedAt)
          .inSeconds
          .clamp(0, 359999);
      add(
        _stat(
          context,
          _elapsed(secs),
          color: color,
          tooltip: '这一轮已经开始 ${_elapsed(secs)}，还没收到可以算速度的用量。',
        ),
      );
    }

    if (showAll && (usage.input > 0 || usage.output > 0)) {
      add(
        _stat(
          context,
          '↑${formatCompactTokens(usage.input)} '
          '↓${formatCompactTokens(usage.output)}',
          color: color,
          tooltip:
              '${live ? '这一轮' : '上一轮'}：输入 ${usage.input} token、'
              '输出 ${usage.output} token。'
              '${usage.rounds > 1 ? '跑了 ${usage.rounds} 次模型调用。' : ''}',
        ),
      );
    }

    final hit = usage.cacheHitRate;
    if (showAll && hit != null) {
      add(
        _stat(
          context,
          '缓存 ${(hit * 100).round()}%',
          color: color,
          // 分母说清楚：命中数是**输入**的子集，不是全部 token 的
          tooltip:
              '输入里有 ${usage.cacheRead} 个 token 直接命中缓存'
              '（占输入的 ${(hit * 100).round()}%）。'
              '${usage.cacheWrite > 0 ? '\n另有 ${usage.cacheWrite} 个输入 token 这一次写进了缓存，下次可以命中。' : ''}',
        ),
      );
    }

    if (percent != null) {
      add(
        _stat(
          context,
          '上下文 $percent%',
          color: color,
          tooltip:
              '最近一次调用的输入 ${usage.lastInput} token ÷ '
              '模型窗口 $window（$percent%）。'
              '${usage.lastModel == null ? '' : '\n模型：${usage.lastModel}'}',
        ),
      );
    }

    if (items.isEmpty) return const SizedBox.shrink();

    return Row(mainAxisSize: MainAxisSize.min, children: items);
  }

  /// 轮内的实时速度估算。`null` = 还算不出来。
  ///
  /// # 为什么它可以是一个估算
  ///
  /// 供应商把 token 数塞在流的最后一帧，段中途没有任何真值可报。而速度
  /// 恰恰是**在生成过程中**才有意义的东西。所以这里拿「这一段的字数 ÷
  /// 这一段的秒数」乘上一个从**上一段真值**学来的「字 / token」比 ——
  /// 界面上带 `~`，段结束就当众换成真数。
  ///
  /// 学不到那个比（本次运行的第一段）时返回 `null`：宁可先不显示，
  /// 也不拿一个跨语言会错四倍的全局常数去填（中文一个字 1–2 token，
  /// 英文四五个字符才一个 token）。
  double? _liveSpeed(StreamingTurn? turn, TurnUsage usage) {
    if (turn == null) return null;
    final startedAt = turn.roundStartedAt;
    final chars = turn.text.length - turn.roundBaseChars;
    final ratio = turn.usage.charsPerToken ?? usage.charsPerToken;
    if (startedAt == null || chars <= 0 || ratio == null) return null;
    final secs = DateTime.now().difference(startedAt).inMilliseconds / 1000;
    // 头半秒算出来的「速度」是噪音（分母太小，第一滴字一出来就是几百）
    if (secs < 0.6) return null;
    return chars / secs * ratio;
  }
}

Widget _stat(
  BuildContext context,
  String text, {
  required Color color,
  required String tooltip,
}) => Tooltip(
  message: tooltip,
  waitDuration: const Duration(milliseconds: 400),
  child: Text(text, style: CortexType.mono(context, size: 11, color: color)),
);

/// `12s` / `1m20s` / `1h02m`。
String _elapsed(int seconds) {
  if (seconds < 60) return '${seconds}s';
  if (seconds < 3600) {
    final m = seconds ~/ 60;
    final s = seconds % 60;
    return '${m}m${s.toString().padLeft(2, '0')}s';
  }
  final h = seconds ~/ 3600;
  final m = (seconds % 3600) ~/ 60;
  return '${h}h${m.toString().padLeft(2, '0')}m';
}
