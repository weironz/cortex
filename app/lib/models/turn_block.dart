import 'json.dart';

/// 一轮回答里的一「块」，按**发生顺序**排。
///
/// # 为什么需要它
///
/// 从前一轮在客户端是两样东西：一整段 `text`，和一个 `toolCalls` 列表。
/// 两者之间的先后**没有任何地方表示**，于是气泡只能画成「一段长文 + 底下
/// 一堆工具」—— 哪怕模型是每调一个工具就说一句话的。
///
/// 2026-09-02 用户实报：「cortex 把所有输出都汇总到底下了」，并指出
/// Claude Code 是「文字和代码修改交错着输出的」。那不是提示词的差别，是
/// **结构**的差别：那边一条 assistant 消息的 `content` 本来就是一个数组，
/// 文本块与工具块是同一个数组里的兄弟，顺序就是下标。
///
/// # 为什么是「骨架」而不是把工具塞进块里
///
/// [ToolBlock] 只带一个下标，指向同一轮的 `toolCalls` —— 工具的数据一份
/// 不复制。两份同源数据要靠人保证一致，而漂了之后的表现是「气泡里那一行
/// 和抽屉里那一行说的不是一回事」。
///
/// # 空 = 不知道，不是「没有块」
///
/// 老服务端、导入进来的历史、以及加这一位之前落库的会话，读回来都是空的。
/// 那时界面**退回从前的画法**（整段正文 + 工具挂底下），而不是画一个空白。
sealed class TurnBlock {
  const TurnBlock();

  static TurnBlock? fromJson(Map<String, dynamic> json) =>
      switch (asStringOrNull(json['type'])) {
        'text' => TextBlock(asString(json['text'])),
        'tool' => ToolBlock(asInt(json['ordinal'])),
        // 认不出的块**整条丢掉**而不是画个占位：以后加了新块类型，老客户端
        // 画一个「未知」比少画一块更糟 —— 它会出现在正文中间。
        _ => null,
      };
}

/// 一段连续的正文。
final class TextBlock extends TurnBlock {
  const TextBlock(this.text);
  final String text;
}

/// 第 [ordinal] 次工具调用（下标进这一轮的 `toolCalls`）。
final class ToolBlock extends TurnBlock {
  const ToolBlock(this.ordinal);
  final int ordinal;
}
