/// **工具调用要能折叠，展开要看得到真实输出。**
///
/// 2026-08-30 用户实报：「cortex 的输出格式直接平铺出来了，而你的都是折叠的，
/// 展开还能看细节」。差的是两件事，这个文件各守一条：
///
/// ① 跑着的时候不再整个摊平 —— 前面完成的折成一行「已完成 N 次」。
/// ② 每一次调用都能展开看到它**真实的输出**，而不只是一句
///    `read_file 返回 28 行 / 815 字符`。
///
/// ② 是端到端的：线协议上新加了 `output`（服务端已截到 2 KB）。少了这一位
/// 界面上永远没有箭头，而那种坏法是最安静的 —— 界面代码全在、一行不报错。
library;

import 'package:cortex_app/features/chat/widgets/turn_drawer.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/core/theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

ToolCall _done(String name, {String? output}) =>
    ToolCall(name: name, result: '返回 3 行', output: output);

Future<void> _pump(WidgetTester tester, Widget child) => tester.pumpWidget(
  ProviderScope(
    child: MaterialApp(
      theme: CortexTheme.light(),
      home: Scaffold(body: SingleChildScrollView(child: child)),
    ),
  ),
);

void main() {
  group('跑着的时候', () {
    testWidgets('完成的调用折成一行，正在跑的那条留着', (tester) async {
      await _pump(
        tester,
        TurnDrawer(
          streaming: true,
          toolCalls: [
            for (var i = 0; i < 6; i++) _done('read_file'),
            const ToolCall(name: 'shell'), // result == null ⇒ 正在跑
          ],
        ),
      );

      expect(
        find.textContaining('已完成 4 次'),
        findsOneWidget,
        reason:
            '六条完成的里该折起 4 条（末尾留 2 条可见）—— 全摊开就是'
            '用户报的「直接平铺出来了」，一轮二十次调用会把回答挤出屏幕',
      );
      expect(
        // 工具名画在 RichText 里（行首还要拼「子 N ·」那一段），
        // 不开 findRichText 的话这条永远找不到
        find.textContaining('shell', findRichText: true),
        findsWidgets,
        reason: '正在跑的那条必须一直可见 —— 它是唯一说明「此刻在等什么」的东西',
      );
    });

    /// 折 1、2 行换来一行「已完成 N 次」，净收益是零还多一次点击。
    testWidgets('少到不值得折的时候就不折', (tester) async {
      await _pump(
        tester,
        TurnDrawer(
          streaming: true,
          toolCalls: [for (var i = 0; i < 3; i++) _done('read_file')],
        ),
      );
      expect(
        find.textContaining('已完成'),
        findsNothing,
        reason: '只有 3 条也去折，用户要多点一次才看得到本来就看得到的东西',
      );
    });
  });

  group('展开看细节', () {
    testWidgets('有输出就给箭头，点开看得到正文', (tester) async {
      await _pump(
        tester,
        TurnDrawer(
          toolCalls: [_done('read_file', output: '这是文件的真实内容')],
          initiallyExpanded: true,
        ),
      );

      final arrow = find.byIcon(Icons.keyboard_arrow_right_rounded);
      expect(
        arrow,
        findsOneWidget,
        reason:
            '有输出却没有箭头 —— 那正是「界面代码全在但线协议少一位」'
            '的样子，一行都不会报错',
      );
      expect(find.textContaining('这是文件的真实内容'), findsNothing);

      await tester.tap(arrow);
      await tester.pumpAndSettle();
      expect(
        find.textContaining('这是文件的真实内容'),
        findsOneWidget,
        reason: '点开了却看不到输出 —— 用户还是只能再问它一遍读到了什么',
      );
    });

    /// **负对照。** 没有输出的行（回放来的、或者本来就没正文）不许给箭头：
    /// 一个点下去空空如也的箭头比没有箭头更让人困惑。
    testWidgets('没有输出的行不给箭头', (tester) async {
      await _pump(
        tester,
        TurnDrawer(toolCalls: [_done('read_file')], initiallyExpanded: true),
      );
      expect(
        find.byIcon(Icons.keyboard_arrow_right_rounded),
        findsNothing,
        reason:
            '打开一个旧会话时 output 是 null（episode_tool_calls 里没有'
            '这一列），那些行本就不该有箭头',
      );
    });
  });

  /// 线协议那一位要真的读得出来。
  ///
  /// 这条盯的是最安静的坏法：字段名对不上 ⇒ 永远 null ⇒ 上面那条「没有输出
  /// 不给箭头」照样绿，而线上一个箭头都不会出现。
  test('output 从线协议里读得出来', () {
    final ev = ChatEvent.fromJson({
      'type': 'tool',
      'name': 'read_file',
      'summary': 'read_file 返回 3 行',
      'output': '真实内容',
      'phase': 'result',
    });
    expect(ev, isA<ChatToolEvent>().having((e) => e.output, 'output', '真实内容'));

    // 不发这个字段的旧服务端 = 没有输出可看，而不是崩
    final old = ChatEvent.fromJson({
      'type': 'tool',
      'name': 'read_file',
      'summary': 'x',
      'phase': 'result',
    });
    expect(old, isA<ChatToolEvent>().having((e) => e.output, 'output', isNull));
  });
}
