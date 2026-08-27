/// 对话区的**注意力分配**：过程弱化、结论正常、长块收起。
///
/// # 这一组盯着三件「不会报错地错」的事
///
/// 1. **工具调用曾经和回答正文一样黑**（`onSurface` + w600），路径还挂着
///    品牌色。于是一屏扫下去最跳眼的是过程，而用户要读的是结论。
///    颜色改回去不会有任何报错 —— 只是界面重新变吵。
/// 2. **长代码块把对话顶出屏幕**。它上下那两句结论才是要读的东西，
///    而一个三百行的块会把它们推到看不见的地方。
/// 3. **正在流式输出的块不能收**。收了的话，用户正看着长的那几行会被
///    藏起来；而 `closed` 变真的那一刻突然塌下去，会把下面的内容整块上移。
library;

import 'package:cortex_app/features/chat/widgets/turn_drawer.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/core/theme.dart';
import 'package:cortex_app/widgets/markdown/code_block.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

Widget _host(Widget child) => MaterialApp(
  home: Scaffold(body: SingleChildScrollView(child: child)),
);

/// 把一棵 RichText 里所有带文字的 span 抽出来。
List<TextSpan> _spansOf(WidgetTester tester, Finder f) {
  final out = <TextSpan>[];
  for (final w in tester.widgetList<RichText>(f)) {
    w.text.visitChildren((s) {
      if (s is TextSpan && s.text != null) out.add(s);
      return true;
    });
  }
  return out;
}

TextSpan _spanWith(WidgetTester tester, Finder f, String needle) =>
    _spansOf(tester, f).firstWhere(
      (s) => s.text!.contains(needle),
      orElse: () => throw StateError('找不到含「$needle」的 span'),
    );

String _long(int lines) =>
    List.generate(lines, (i) => 'let x$i = $i;').join('\n');

void main() {
  group('过程弱化、结论正常', () {
    /// ⚠️ 这一条是这组里最要紧的：工具名此前用的就是 `onSurface`，
    /// 与回答正文**同一档**。两者一样黑的时候，眼睛分不出该先读哪个。
    testWidgets('工具名比正文退一档，不与结论抢注意力', (tester) async {
      await tester.pumpWidget(
        _host(
          const TurnDrawer(
            initiallyExpanded: true,
            toolCalls: [ToolCall(name: 'read_file', path: 'src/a.rs')],
          ),
        ),
      );

      final theme = Theme.of(tester.element(find.byType(TurnDrawer)));
      final name = _spanWith(tester, find.byType(RichText), 'read_file');
      expect(
        name.style!.color,
        isNot(theme.colorScheme.onSurface),
        reason: '与回答正文同色的话，一屏扫下去最跳眼的是过程而不是结论',
      );
      expect(name.style!.color, theme.colorScheme.onSurfaceVariant);
    });

    /// ⚠️ 路径此前是 `accentInk`（品牌色）。每一条工具行都挂一块品牌色，
    /// 整段过程于是比结论还显眼 —— 而「这是个路径」既不是动作也不是含义，
    /// 按主题那条第一原则就不该用彩色。
    testWidgets('路径不再用品牌色，改用字重把它认出来', (tester) async {
      await tester.pumpWidget(
        _host(
          const TurnDrawer(
            initiallyExpanded: true,
            toolCalls: [ToolCall(name: 'write_file', path: 'src/notes.md')],
          ),
        ),
      );

      final theme = Theme.of(tester.element(find.byType(TurnDrawer)));
      final path = _spanWith(tester, find.byType(RichText), 'src/notes.md');
      expect(
        path.style!.color,
        isNot(theme.cortex.accentInk),
        reason: '品牌色是给动作与含义的，不是给「这是个路径」的',
      );
      expect(
        path.style!.fontWeight,
        FontWeight.w500,
        reason: '拿掉颜色之后，路径要靠字重才认得出来 —— 两样都拿掉就真看不见了',
      );
    });

    /// 参数与结果比工具名再低一档。参数常是被截断的，读它不如读结果。
    testWidgets('参数压到第三级，比工具名还淡', (tester) async {
      await tester.pumpWidget(
        _host(
          const TurnDrawer(
            initiallyExpanded: true,
            toolCalls: [ToolCall(name: 'shell', arguments: '(cmd=cargo test)')],
          ),
        ),
      );

      final theme = Theme.of(tester.element(find.byType(TurnDrawer)));
      final args = _spanWith(tester, find.byType(RichText), 'cargo test');
      expect(args.style!.color, theme.cortex.foregroundTertiary);
    });

    /// **失败仍然是错误色。** 弱化的是「过程」，不是「出事了」——
    /// 把失败也一起压灰的话，这次弱化就把唯一需要注意的东西也埋了。
    testWidgets('失败那一行不跟着弱化', (tester) async {
      await tester.pumpWidget(
        _host(
          const TurnDrawer(
            initiallyExpanded: true,
            toolCalls: [
              ToolCall(name: 'shell', failed: true, result: 'exit code 1'),
            ],
          ),
        ),
      );

      final theme = Theme.of(tester.element(find.byType(TurnDrawer)));
      expect(
        _spanWith(tester, find.byType(RichText), 'exit code 1').style!.color,
        theme.colorScheme.error,
        reason: '弱化的是过程不是状态；失败一起压灰等于把唯一要注意的东西埋了',
      );
    });

    /// 增删数是这一行里**唯一保留彩色**的东西 —— 它是含义，
    /// 而且正是用来决定「要不要点开」的。
    testWidgets('有改动的行把增删数摆在行上', (tester) async {
      await tester.pumpWidget(
        _host(
          const TurnDrawer(
            initiallyExpanded: true,
            toolCalls: [
              ToolCall(
                name: 'write_file',
                path: 'a.rs',
                diff: ' keep\n+one\n+two\n-gone\n',
              ),
            ],
          ),
        ),
      );

      expect(
        _spansOf(tester, find.byType(RichText)).map((s) => s.text),
        containsAll(<String>['+2', '−1']),
        reason: '「这次改动大不大」此前要先点开箭头再目测一屏绿红',
      );
    });

    testWidgets('没有改动的行不摆增删数', (tester) async {
      await tester.pumpWidget(
        _host(
          const TurnDrawer(
            initiallyExpanded: true,
            toolCalls: [ToolCall(name: 'read_file', path: 'a.rs')],
          ),
        ),
      );
      expect(
        _spansOf(tester, find.byType(RichText)).map((s) => s.text),
        isNot(contains('+0')),
        reason: '一个恒为 +0 −0 的角标只是噪音',
      );
    });
  });

  group('长代码块收起', () {
    testWidgets('短块不收，也不摆展开按钮', (tester) async {
      await tester.pumpWidget(
        _host(CodeBlock(code: _long(4), language: 'rust')),
      );
      // ⚠️ 两个字都要断言。只查「展开全部」的话这条是**空的**：
      // 短块本来就是展开态，那个把手写的是「收起」—— 于是把
      // `_collapsible` 改成恒真也照样绿（2026-08-27 故障注入验出来的）
      expect(
        find.textContaining('展开全部'),
        findsNothing,
        reason: '一个四行的块摆展开按钮，等于每段代码都多一行没用的把手',
      );
      expect(find.text('收起'), findsNothing, reason: '同上 —— 短块整个不该有这一条');
    });

    testWidgets('长块默认收起，并说清共多少行', (tester) async {
      await tester.pumpWidget(
        _host(CodeBlock(code: _long(60), language: 'rust')),
      );
      expect(
        find.text('展开全部 · 共 60 行'),
        findsOneWidget,
        reason: '不写数字的话，人无法判断底下是十行还是三百行 —— 也就无法判断该不该点',
      );
    });

    testWidgets('点一下展开，再点收回去', (tester) async {
      await tester.pumpWidget(
        _host(CodeBlock(code: _long(60), language: 'rust')),
      );
      await tester.tap(find.textContaining('展开全部'));
      await tester.pump();
      expect(find.text('收起'), findsOneWidget);

      // 展开之后这一块有六十行高，「收起」被顶到视口外面了 ——
      // `find` 找得到它（找不看可见性），而 `tap` 会打在屏幕外什么都碰不到。
      // 这正是它想验证的那件事的一部分：长块展开后要能滚到底再收回去
      await tester.ensureVisible(find.text('收起'));
      await tester.pump();
      await tester.tap(find.text('收起'));
      await tester.pump();
      expect(find.textContaining('展开全部'), findsOneWidget);
    });

    /// ⚠️ 正在流式输出时**不收**：那时用户正看着它长。
    testWidgets('还在输出的块不收起', (tester) async {
      await tester.pumpWidget(
        _host(CodeBlock(code: _long(60), language: 'rust', closed: false)),
      );
      expect(
        find.textContaining('展开全部'),
        findsNothing,
        reason: '收起会把刚吐出来的几行藏起来',
      );
    });

    /// ⚠️ 而且流完之后也不能**突然**塌下去 —— 那会把下面的内容整块上移，
    /// 位移正是最难受的一类动效。在眼前长过的块，此后一直摊着。
    testWidgets('流完之后不会自己塌下去', (tester) async {
      await tester.pumpWidget(
        _host(CodeBlock(code: _long(60), language: 'rust', closed: false)),
      );
      // 同一个 State：只把 closed 翻成真，模拟收尾的那个 fence 到货
      await tester.pumpWidget(
        _host(CodeBlock(code: _long(60), language: 'rust')),
      );
      expect(
        find.text('收起'),
        findsOneWidget,
        reason: '在眼前长出来的块要保持摊开，但仍要给一个「收起」把手',
      );
    });
  });
}
