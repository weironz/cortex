import 'package:cortex_app/core/theme.dart';
import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/widgets/markdown/code_block.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// Replays a token stream through the real widget tree.
///
/// The reply is a fixture rather than a live completion on purpose: this suite
/// is about the *renderer's* behaviour under an incomplete document, and
/// driving it with whatever the model felt like writing made a highlighter
/// regression indistinguishable from a chatty model. The live counterpart in
/// `live_render_test.dart` checks the wire, not the fence.
const _reply = '''
结论：**默认用原生 `async fn` in trait。**

```rust
// 热路径静态分发，不掏堆
pub trait Provider {
    async fn complete(&self, req: Request) -> Result<Response, ProviderError>;
}
```

需要 `dyn` 分发时才退回宏。
''';

/// Chunked over runes, not code units — slicing a surrogate pair would emit a
/// lone half and render as a replacement glyph mid-stream.
List<String> _chunks(String text, {int size = 5}) {
  final runes = text.runes.toList(growable: false);
  return [
    for (var i = 0; i < runes.length; i += size)
      String.fromCharCodes(
        runes.sublist(i, i + size > runes.length ? runes.length : i + size),
      ),
  ];
}

void main() {
  testWidgets('未闭合的围栏从第一个 token 起就是代码块，且元素不被重建', (tester) async {
    tester.view.physicalSize = const Size(1200, 1600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    Widget frame(String text, bool streaming) => MaterialApp(
      theme: CortexTheme.dark(),
      home: Scaffold(
        body: SingleChildScrollView(
          child: AssistantBlock(
            text: text,
            facts: const [],
            toolCalls: const [],
            streaming: streaming,
          ),
        ),
      ),
    );

    var shown = '';
    Element? codeBlockElement;
    var sawOpenFence = false;

    for (final chunk in _chunks(_reply)) {
      shown += chunk;
      await tester.pumpWidget(frame(shown, true));
      await tester.pump(const Duration(milliseconds: 16));

      final found = find.byType(CodeBlock);
      if (found.evaluate().isEmpty) continue;

      final block = tester.widget<CodeBlock>(found.first);
      if (!block.closed) sawOpenFence = true;

      // Element identity must persist across deltas — that is what makes the
      // bubble grow in place instead of being rebuilt (whose visible symptom
      // would be a flicker and a scroll jump).
      codeBlockElement ??= found.evaluate().first;
      expect(
        found.evaluate().first,
        same(codeBlockElement),
        reason: '代码块必须是被更新，而不是被重建',
      );
    }

    expect(sawOpenFence, isTrue, reason: '围栏没闭合时就该以代码块渲染，而不是先当字面反引号、之后突然翻转');

    await tester.pumpWidget(frame(shown, false));
    await tester.pump(const Duration(milliseconds: 50));

    final block = tester.widget<CodeBlock>(find.byType(CodeBlock));
    expect(block.language, 'rust');
    expect(block.closed, isTrue);
    expect(block.code, contains('async fn complete'));

    // Genuinely tokenised: walk the rendered span tree and confirm the code is
    // painted in more than one colour. Counting top-level children would not
    // work — `Text.rich` wraps the supplied span in a single-child root.
    final codeSpan = find
        .descendant(of: find.byType(CodeBlock), matching: find.byType(RichText))
        .evaluate()
        .map((e) => (e.widget as RichText).text)
        .firstWhere((s) => s.toPlainText().contains('async fn complete'));

    // `visitChildren` already descends the whole tree — recursing inside the
    // visitor overflows the stack.
    final colours = <Color>{};
    codeSpan.visitChildren((span) {
      final colour = span.style?.color;
      if (colour != null) colours.add(colour);
      return true;
    });
    expect(
      colours.length,
      greaterThan(1),
      reason: 'Rust 必须多色渲染，只有一种颜色说明降级成纯文本了：$colours',
    );
  });
}
