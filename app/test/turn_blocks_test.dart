/// **文字与工具要交错着画，不是「一段长文 + 底下一堆工具」。**
///
/// 2026-09-02 用户实报：「cortex 把所有输出都汇总到底下了」，并指出
/// Claude Code 是「文字和代码修改交错着输出的」。
///
/// 那不是提示词的差别，是**结构**的差别：从前一轮在客户端是两样东西 ——
/// 一整段 `text` 和一个 `toolCalls` 列表，两者之间的先后**没有任何地方
/// 表示**。所以哪怕模型每调一个工具就说一句，画出来也必然是一段长文加
/// 一个抽屉。
///
/// 修法是给一轮加一条「骨架」：按发生顺序排的块，文本块与工具块是同一个
/// 列表里的兄弟 —— 与 Anthropic 那边 `content: [text, tool_use, …]` 同一个
/// 形状。工具数据一份不复制，块里只带下标。
library;

import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/core/theme.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/models/turn_block.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> _pump(WidgetTester tester, Widget child) => tester.pumpWidget(
  ProviderScope(
    child: MaterialApp(
      theme: CortexTheme.light(),
      home: Scaffold(body: SingleChildScrollView(child: child)),
    ),
  ),
);

/// 屏幕上从上到下的 y 坐标 —— 判「谁在谁上面」只能靠它。
double _top(WidgetTester tester, Finder f) => tester.getTopLeft(f).dy;

void main() {
  final tools = [
    ToolCall(name: 'read_file', result: '返回 3 行', path: 'a.rs'),
    ToolCall(name: 'grep', result: '返回 1 行', path: 'b.rs'),
  ];

  testWidgets('有骨架时，工具画在它对应的那段话之后', (tester) async {
    await _pump(
      tester,
      AssistantBlock(
        text: '先看第一个文件。看完再搜。收工。',
        toolCalls: tools,
        blocks: const [
          TextBlock('先看第一个文件。'),
          ToolBlock(0),
          TextBlock('看完再搜。'),
          ToolBlock(1),
          TextBlock('收工。'),
        ],
        createdAt: DateTime.utc(2026, 9, 2),
      ),
    );

    final firstText = find.textContaining('先看第一个文件');
    final midText = find.textContaining('看完再搜');
    final lastText = find.textContaining('收工');
    final readRow = find.textContaining('read_file', findRichText: true);
    final grepRow = find.textContaining('grep', findRichText: true);

    for (final f in [firstText, midText, lastText, readRow, grepRow]) {
      expect(f, findsWidgets, reason: '这一块根本没画出来：$f');
    }

    // ★ 判据是**屏幕上的先后**，不是「都出现了」。
    // 只断言「都在」的话，全部挤在底下的旧画法照样绿 —— 那正是要修的东西。
    expect(
      _top(tester, readRow.first),
      greaterThan(_top(tester, firstText.first)),
      reason: 'read_file 那一行没有跟在它对应的那句话后面',
    );
    expect(
      _top(tester, midText.first),
      greaterThan(_top(tester, readRow.first)),
      reason:
          '第二段话跑到了第一次工具调用**前面** —— 那就是「一段长文 + '
          '底下一堆工具」的样子，正文全在上、工具全在下',
    );
    expect(
      _top(tester, grepRow.first),
      greaterThan(_top(tester, midText.first)),
      reason: 'grep 那一行没有跟在第二段话后面',
    );
    expect(
      _top(tester, lastText.first),
      greaterThan(_top(tester, grepRow.first)),
      reason: '收尾那句话没有落在最后一次调用之后',
    );
  });

  /// **负对照。** 骨架为空（老服务端、导入的历史、加这一位之前的会话）
  /// 时必须退回从前的画法，而不是画一个空白的回答。
  testWidgets('没有骨架时退回从前的画法：正文在上，工具在下', (tester) async {
    await _pump(
      tester,
      AssistantBlock(
        text: '一段完整的回答。',
        toolCalls: tools,
        createdAt: DateTime.utc(2026, 9, 2),
      ),
    );

    final text = find.textContaining('一段完整的回答');
    // 退回的画法里抽屉是**折叠**的（跑完之后收起来），所以找的是那行标题
    // 而不是工具名 —— 工具名要展开才画，那是既有行为
    final drawer = find.textContaining('本轮工具调用');
    expect(text, findsWidgets, reason: '骨架为空时正文不见了 —— 老会话会变成一片空白');
    expect(drawer, findsWidgets, reason: '骨架为空时工具也得画，只是收在底下那个抽屉里');
    expect(
      _top(tester, drawer.first),
      greaterThan(_top(tester, text.first)),
      reason: '退回的画法就是「正文在上、工具在下」',
    );
  });

  /// **同一次调用不许画两遍。**
  ///
  /// 骨架把工具画进了正文中间，底下那个抽屉就得让位 —— 两处都画的话，
  /// 用户会以为它跑了两回。
  testWidgets('有骨架时，底下的抽屉不再重复画一遍', (tester) async {
    await _pump(
      tester,
      AssistantBlock(
        text: '只调一次。',
        toolCalls: [tools.first],
        blocks: const [TextBlock('只调一次。'), ToolBlock(0)],
        createdAt: DateTime.utc(2026, 9, 2),
      ),
    );
    expect(
      find.textContaining('read_file', findRichText: true),
      findsOneWidget,
      reason: '同一次调用出现了两次 —— 一次在正文中间，一次在底下的抽屉里',
    );
  });

  /// **下标越界什么都不画。**
  ///
  /// 骨架与工具列表对不上是真实存在的（拼过列表的那种历史消息、老数据）。
  /// 那时宁可少画一行，也不能在正文中间冒出一个指错的工具 —— 后者不报错，
  /// 只是说了假话。
  testWidgets('骨架与工具列表对不上时，宁可少画也不指错', (tester) async {
    await _pump(
      tester,
      AssistantBlock(
        text: '正文还在。',
        toolCalls: [tools.first],
        blocks: const [TextBlock('正文还在。'), ToolBlock(7)],
        createdAt: DateTime.utc(2026, 9, 2),
      ),
    );
    expect(find.textContaining('正文还在'), findsWidgets, reason: '一个越界的块把整条消息带崩了');
    expect(
      find.textContaining('read_file', findRichText: true),
      findsNothing,
      reason: '越界的序号指到了别的工具上 —— 那是在正文中间说假话',
    );
  });

  /// 线协议那一位要真的读得出来。
  ///
  /// 最安静的坏法：字段名对不上 ⇒ 永远解析不出 ⇒ 上面那条负对照照样绿，
  /// 而线上一条交错都不会出现。
  test('块从线协议里读得出来，认不出的整条丢掉', () {
    expect(
      TurnBlock.fromJson({'type': 'text', 'text': '你好'}),
      isA<TextBlock>().having((b) => b.text, 'text', '你好'),
    );
    expect(
      TurnBlock.fromJson({'type': 'tool', 'ordinal': 2}),
      isA<ToolBlock>().having((b) => b.ordinal, 'ordinal', 2),
    );
    expect(
      TurnBlock.fromJson({'type': '以后才有的块'}),
      isNull,
      reason: '认不出的块该整条丢掉 —— 画个「未知」占位会出现在正文中间',
    );
  });
}
