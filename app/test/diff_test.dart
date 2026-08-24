/// diff 的渲染与汇总。
///
/// # 为什么这几条值得写
///
/// 着色错了不会报错，只是一屏看不出增删的文本 —— 而它服务的那个决定
/// （按不按「允许」）恰恰全靠增删。汇总口径错了同样安静：面板上的数字
/// 与实际改动对不上，而人正是拿那个数字判断「这次改动大不大」。
library;

import 'package:cortex_app/features/chat/session_changes_sheet.dart';
import 'package:cortex_app/features/chat/widgets/diff_view.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/core/theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const _diff = '''
 keep this
-old line
+new line
@@
 tail
''';

ChatMessage _msg(List<ToolCall> calls) => ChatMessage(
  id: 'm1',
  role: MessageRole.assistant,
  text: 'ok',
  createdAt: DateTime.utc(2026, 8, 13),
  toolCalls: calls,
);

void main() {
  group('增删计数', () {
    test('只数内容行，@@ 与说明不算改动', () {
      final c = countChanges(_diff);
      expect(c.added, 1);
      expect(c.removed, 1);
    });

    test('截断说明那行不被算成改动', () {
      const withNote = '+a\n…（只显示了前 400 行；其余未显示）\n';
      final c = countChanges(withNote);
      expect(
        c.added,
        1,
        reason:
            '截断说明是元信息不是改动。算进去的话，'
            '一份被截断的 diff 会凭空多出一行「新增」',
      );
    });

    test('空 diff 计数为零，不崩', () {
      final c = countChanges('');
      expect(c.added, 0);
      expect(c.removed, 0);
    });
  });

  group('按文件汇总', () {
    test('只收有 diff 的调用 —— 读文件与 shell 不是改动', () {
      final files = groupChangesByFile([
        _msg([
          const ToolCall(name: 'read_file', path: 'a.txt', result: '读到了'),
          const ToolCall(name: 'shell', result: '跑完了'),
          const ToolCall(name: 'write_file', path: 'b.txt', diff: _diff),
        ]),
      ]);
      expect(files.map((f) => f.path), [
        'b.txt',
      ], reason: '把只读操作也列进「改动」，用户会以为 agent 动了它没动的文件');
    });

    test('同一个文件改两次是两段，不合并', () {
      final files = groupChangesByFile([
        _msg([
          const ToolCall(name: 'write_file', path: 'a.txt', diff: '+1\n'),
          const ToolCall(name: 'write_file', path: 'a.txt', diff: '+2\n'),
        ]),
      ]);
      expect(files, hasLength(1));
      expect(
        files.single.diffs,
        hasLength(2),
        reason:
            '合并成一份「最终 diff」会把「它改了两次」这个事实抹掉，'
            '而那正是回头看时最想知道的事情之一',
      );
      expect(files.single.totals.added, 2);
    });

    test('按首次出现排序，不按字母', () {
      final files = groupChangesByFile([
        _msg([
          const ToolCall(name: 'write_file', path: 'z.txt', diff: '+1\n'),
          const ToolCall(name: 'write_file', path: 'a.txt', diff: '+1\n'),
        ]),
      ]);
      expect(files.map((f) => f.path), [
        'z.txt',
        'a.txt',
      ], reason: '人记得的是「先改了 z 再改了 a」。按字母重排会让这个顺序消失');
    });

    test('没有任何改动时是空列表，不是崩溃', () {
      expect(groupChangesByFile(const []), isEmpty);
      expect(groupChangesByFile([_msg(const [])]), isEmpty);
    });
  });

  group('DiffView 渲染', () {
    testWidgets('增删上下文三种行各自着色', (tester) async {
      await tester.pumpWidget(
        const MaterialApp(home: Scaffold(body: DiffView(_diff))),
      );

      Color colorOf(String text) =>
          tester.widget<Text>(find.text(text)).style!.color!;

      // 2026-08-24 起取主题 token（深浅各配一档），不再是写死的单值 ——
      // 旧的中间调在两种主题下都不到 4.5:1，而 diff 恰恰出现在确认框里
      final theme = Theme.of(tester.element(find.byType(DiffView)));
      expect(
        colorOf('+new line'),
        theme.cortex.success,
        reason: '绿加红减是这件事的通用语言（git、GitHub、每个 code review 工具）',
      );
      expect(colorOf('-old line'), theme.colorScheme.error);
      expect(
        colorOf('+new line'),
        isNot(colorOf(' keep this')),
        reason: '新增行与上下文行同色的话，这份 diff 就退化成一段普通文本',
      );
    });

    testWidgets('@@ 与截断说明压暗 —— 它们说的是「这里省略了东西」', (tester) async {
      await tester.pumpWidget(
        const MaterialApp(home: Scaffold(body: DiffView(_diff))),
      );
      final meta = tester.widget<Text>(find.text('@@')).style!.color;
      final ctx = tester.widget<Text>(find.text(' tail')).style!.color;
      expect(meta, isNot(ctx));
    });
  });
}
