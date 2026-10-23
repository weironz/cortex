import 'chat_event.dart';

/// 一轮对话的用量账本。
///
/// # 这里不做任何估算
///
/// 每一项都来自供应商的报告（[ChatUsageEvent]）。它没报的项就是「不知道」，
/// 界面据此整项不画 —— 而不是拿一个全局平均值填上去。唯一一处估算
/// （[charsPerToken]）只在**轮内**用来让速度那个数字动起来，段一结束就按
/// 真值校正。
class TurnUsage {
  const TurnUsage({
    this.input = 0,
    this.output = 0,
    this.cacheRead = 0,
    this.cacheWrite = 0,
    this.cacheReported = false,
    this.timedOutput = 0,
    this.timedMs = 0,
    this.rounds = 0,
    this.lastInput,
    this.lastModel,
    this.charsPerToken,
    this.steps = 0,
  });

  /// 输入合计（**含**缓存部分）。子 agent 烧的也算进来 —— 那是真花掉的钱。
  final int input;
  final int output;
  final int cacheRead;
  final int cacheWrite;

  /// 有没有**任何一段**报过缓存数。
  ///
  /// 光看 `cacheRead > 0` 分不出两种情形：供应商如实报了 0（这一轮一次都没
  /// 命中）与它压根不说，累加起来都是 0。而界面上「缓存 0%」与干脆不画是
  /// 两句完全不同的话 —— 前者是事实，后者是我们不知道。
  final bool cacheReported;

  /// 只累加**能量出生成耗时**的那几段：速度的分母是它，不是 [rounds]。
  ///
  /// 把量不出耗时的段数算进分母，速度会被压成无关的数。
  final int timedOutput;
  final int timedMs;

  /// 这一轮跑了几次模型调用。
  final int rounds;

  /// 最近一次**主 agent** 调用的输入 token —— 上下文填充率的分子。
  ///
  /// 子 agent 那几条不算进来：它们的提示词是一小段任务描述，与这条会话的
  /// 历史无关，混进来的话百分比会在一轮里来回跳。
  final int? lastInput;

  /// 最近一次主 agent 调用实际用的模型（查上下文窗口用）。
  final String? lastModel;

  /// 客户端自己数出来的字数 ÷ 供应商报的输出 token。
  ///
  /// 只用于轮内的实时估算：供应商要到这一**段**结束才报 token 数，而界面
  /// 上那个速度在生成过程中就该动。段结束后按真值校正（真值优先）。
  final double? charsPerToken;

  /// 这一轮调了几次工具。
  final int steps;

  bool get isEmpty => rounds == 0;

  /// 整轮的平均生成速度。
  ///
  /// `null` = 一段都量不出耗时（全是一个增量就完的），不是 0 ——
  /// 界面据此不画，而不是画一个「0 tok/s」。
  double? get tokensPerSecond =>
      timedMs > 0 && timedOutput > 0 ? timedOutput * 1000 / timedMs : null;

  /// 缓存命中率：命中数 ÷ 总输入。
  ///
  /// `null` = 供应商一次都没报过缓存（[cacheReported]），或者压根没有输入。
  /// **报过 0 就画 0%** —— 那是一句事实（这一轮一次都没命中），与「这家
  /// 供应商不说」必须分得开。
  double? get cacheHitRate {
    if (!cacheReported || input <= 0) return null;
    // 有的兼容层只报缓存数、不报输入，两者口径对不上时算出来会超过 100%。
    // 那是「这个数不可信」，不是「缓存命中了 120%」
    return (cacheRead / input).clamp(0.0, 1.0);
  }

  /// 收下一条用量事件。
  ///
  /// [chars] 是客户端在这一段里数到的字数 —— 只用来学 [charsPerToken]。
  TurnUsage add(ChatUsageEvent event, {int chars = 0}) {
    final out = event.outputTokens ?? 0;
    final measurable = out > 0 && event.generationMs > 0;
    // 子 agent 的那几条只进总数，不进「这条会话的上下文」。
    //
    // 字/token 比也只能从主 agent 的段上学：字数是从主 agent 那条正文里数
    // 出来的，拿它对子 agent 的 token，学出来的是一个两边都不成立的比值
    final byMain = event.subagent == null;
    final ratio = byMain && out > 0 && chars > 0 ? chars / out : null;
    return TurnUsage(
      input: input + (event.inputTokens ?? 0),
      output: output + out,
      cacheRead: cacheRead + (event.cacheReadInputTokens ?? 0),
      cacheWrite: cacheWrite + (event.cacheWriteInputTokens ?? 0),
      // 报过 0 也算报过 —— 见 [cacheReported]
      cacheReported:
          cacheReported ||
          event.cacheReadInputTokens != null ||
          event.cacheWriteInputTokens != null,
      // 速度只从**主 agent** 的段上算。子 agent 是几路并行跑的，它们的
      // 生成时间互相重叠 —— 加进分母等于把同一段时间算了两遍，而界面上
      // 那个数说的是「你看着的这段正文吐得多快」
      timedOutput: timedOutput + (measurable && byMain ? out : 0),
      timedMs: timedMs + (measurable && byMain ? event.generationMs : 0),
      rounds: rounds + 1,
      // 它没报输入就沿用上一次的：一个「上下文 —」比一个旧值更没用
      lastInput: byMain ? (event.inputTokens ?? lastInput) : lastInput,
      lastModel: byMain ? (event.model ?? lastModel) : lastModel,
      charsPerToken: ratio ?? charsPerToken,
      steps: steps,
    );
  }

  /// 记下这一轮调了几次工具。轮次结束时由控制器补上（它才知道最终条数）。
  TurnUsage withSteps(int n) => TurnUsage(
    input: input,
    output: output,
    cacheRead: cacheRead,
    cacheWrite: cacheWrite,
    cacheReported: cacheReported,
    timedOutput: timedOutput,
    timedMs: timedMs,
    rounds: rounds,
    lastInput: lastInput,
    lastModel: lastModel,
    charsPerToken: charsPerToken,
    steps: n,
  );
}

/// 把 token 数压成 `12.3k` / `1.2M`。
///
/// 七位数原样画出来读不出量级，而状态栏一共就那么点地方。
///
/// 与 `usage_report.dart` 的 [formatTokens]（带千位分隔的完整数）分工不同：
/// 那个用在设置页的用量面板上，那里有整行宽度、要的是精确。
String formatCompactTokens(int n) {
  final abs = n.abs();
  if (abs < 1000) return n.toString();
  if (abs < 1000 * 1000) return '${(n / 1000).toStringAsFixed(1)}k';
  if (abs < 1000 * 1000 * 1000) return '${(n / 1000000).toStringAsFixed(1)}M';
  return '${(n / 1000000000).toStringAsFixed(1)}B';
}

/// 上下文填了百分之几。`null` = 不知道窗口多大，不画。
int? contextPercent(int? usedTokens, int? windowTokens) {
  if (usedTokens == null || usedTokens <= 0) return null;
  if (windowTokens == null || windowTokens <= 0) return null;
  // 超过 100% 是可能的（服务端在折叠之前就已经发出去了），据实画，
  // 但不要画出 3000% 这种把整行吃掉的东西
  return (usedTokens * 100 / windowTokens).round().clamp(0, 999);
}
