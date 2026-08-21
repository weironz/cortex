/// 型号的显示规则 —— 徽标、上下文、价格。
///
/// # 为什么单独一份
///
/// 这些判据出现在三个地方（模型列表、选择器、默认模型那一页），
/// **各画各的下场是同一个型号在两个界面上说法不一** ——
/// 比如「弹窗里说能生图、详情页里不说」。
///
/// 它们原本长在 `model_add_dialog.dart` 里（那个「拉完列表再勾选」的弹窗）。
/// 2026-08-21 模型页照 LobeHub 重做之后，勾选这件事挪进了列表本身
/// （每个型号一个开关），弹窗退役，而这几个函数还得留着。
library;

import 'package:flutter/material.dart';

import '../../../models/model_option.dart';
import '../../../models/model_source.dart';

/// 一个型号的能力徽标。
///
/// **查不到的不画** —— 画一个灰的会被读成「不支持」，而实际是「不知道」。
List<(IconData, String)> badgesOf(FetchedModel m) => [
  if (m.toolCall == true) (Icons.build_outlined, '支持工具调用，能跑 agent'),
  if (m.vision == true) (Icons.visibility_outlined, '看得懂图'),
  if (m.imageOutput == true) (Icons.brush_outlined, '能生图'),
  if (m.reasoning == true) (Icons.lightbulb_outline, '有思考过程'),
];

/// 同上，但输入是选择器那份目录里的 [ModelOption]。
///
/// 与 [badgesOf] 挨着放：**判据只此一处**。两个地方各画各的徽标，
/// 迟早出现「弹窗里说能生图、详情页里不说」。
List<(IconData, String)> badgesOfOption(ModelOption? m) => m == null
    ? const []
    : [
        if (m.toolCall == true) (Icons.build_outlined, '支持工具调用，能跑 agent'),
        if (m.vision == true) (Icons.visibility_outlined, '看得懂图'),
        if (m.reasoning == true) (Icons.lightbulb_outline, '有思考过程'),
      ];

/// 上下文窗口的短写法：`128000` → `128K`。
///
/// 查不到时回**空串**而不是 `—`：一个横杠在一列数字里读起来像
/// 「零」或者「不支持」，而事实是「服务端目录里没有这个型号」。
/// 调用方据空串决定整个徽标画不画。
String formatContextTokens(int? tokens) {
  if (tokens == null || tokens <= 0) return '';
  if (tokens >= 1000000) {
    final m = tokens / 1000000;
    return '${m == m.roundToDouble() ? m.toInt() : m.toStringAsFixed(1)}M';
  }
  if (tokens >= 1000) return '${(tokens / 1000).round()}K';
  return '$tokens';
}

/// 每百万 token 的美元微元 → 人看得懂的价格。空串 = 没有价目。
String formatUsdPerMtok(int? micros) {
  if (micros == null) return '';
  final usd = micros / 1000000;
  if (usd == 0) return r'$0';
  if (usd < 0.01) return '\$${usd.toStringAsFixed(4)}';
  return '\$${usd.toStringAsFixed(2)}';
}

/// 一行价格。两头都查不到就回空串 —— **不画半行**。
///
/// 只有输入价没有输出价（或反过来）在目录里是不会出现的，
/// 真出现了也说明这条目录记录是坏的，那时少说一句比说半句强。
String formatPricePair(FetchedModel m) {
  final input = formatUsdPerMtok(m.inputMicrosPerMtok);
  final output = formatUsdPerMtok(m.outputMicrosPerMtok);
  if (input.isEmpty || output.isEmpty) return '';
  return '输入 $input/M · 输出 $output/M';
}

/// 按**系列**把型号分组：`qwen-image-3.0` 与 `qwen-turbo` 都进 `qwen`。
///
/// 240 个型号铺成一条平列表没法看，而同一系列的东西本来就该待在一起。
/// 组内与组间都保持服务端给的顺序 —— 那是它 `/models` 的顺序，
/// 重排一次就多一个「我记得它在上面」的困惑。
///
/// ## 它去过又回来了
///
/// 2026-08-21 上午照 LobeHub 改成平铺时删掉过（那份列表是平的），当天下午
/// 又照 Cherry Studio 改回来 —— 两份列表（主列表与选型抽屉）都按系列分组。
/// 留着这段是想说清：**分组解决的是「240 个型号铺成一条平列表」**，
/// 而那个问题一直在，只是被搜索框与页签**分担**过一阵子。
List<(String, List<T>)> groupByFamily<T>(
  List<T> items,
  String Function(T) idOf,
) {
  final out = <(String, List<T>)>[];
  for (final m in items) {
    final id = idOf(m);
    final i = id.indexOf('-');
    final family = i <= 0 ? id : id.substring(0, i);
    final at = out.indexWhere((g) => g.$1 == family);
    if (at < 0) {
      out.add((family, <T>[m]));
    } else {
      out[at].$2.add(m);
    }
  }
  return out;
}

/// 型号的模态分类。**只有我们真的判得出的那几档。**
///
/// LobeHub 那一排页签有 `视频 / 向量化 / ASR / TTS`，我们四样数据都没有 ——
/// 画四个恒为 0 的页签，是在说一个不成立的能力（CLAUDE.md 不可违反约束
/// 第 2 条的界面版本）。所以这里只有两档，加上「全部」。
enum ModelKind {
  /// 能画图。`imageUnwired` 也算：那是「它会画，但我们还没接这家」，
  /// 归到「图片」里比归到「对话」里离事实近得多。
  image('图片', Icons.image_outlined),

  /// 其余的都算对话。**不是「我们确认它能聊天」** ——
  /// 是「它不是一个专门画图的」，这一点我们判得出。
  chat('对话', Icons.chat_bubble_outline);

  const ModelKind(this.label, this.icon);

  final String label;
  final IconData icon;

  static ModelKind of(FetchedModel m) =>
      (m.imageOutput == true || m.imageUnwired)
      ? ModelKind.image
      : ModelKind.chat;
}
