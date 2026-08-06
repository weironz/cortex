@Tags(['live'])
library;

import 'dart:io';

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/theme.dart';
import 'package:cortex_app/features/chat/widgets/memory_drawer.dart';
import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/memory_fact.dart';
import 'package:cortex_app/widgets/markdown/code_block.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// Renders a **real** `cortexd` reply through the real widget tree.
///
/// `TestWidgetsFlutterBinding` replaces `HttpOverrides.global` with a stub that
/// returns 400 for everything, so the fetch is wrapped in
/// [HttpOverrides.runZoned] with a genuine `HttpClient` — a zone-local override
/// wins over the global one.
const _baseUrl = 'http://127.0.0.1:8080';

Future<bool> _daemonUp() async {
  try {
    final socket = await Socket.connect(
      '127.0.0.1',
      8080,
      timeout: const Duration(milliseconds: 600),
    );
    socket.destroy();
    return true;
  } on Object {
    return false;
  }
}

/// A zone-local override that overrides nothing.
///
/// `HttpOverrides.createHttpClient` in the base class constructs the platform
/// client directly, so installing a bare subclass shadows the test binding's
/// stub and restores real networking. Doing it via
/// `runZoned(createHttpClient: () => HttpClient())` instead would recurse
/// infinitely — that factory re-enters the override it is installing.
class _PassthroughHttpOverrides extends HttpOverrides {}

Future<T> _withRealHttp<T>(Future<T> Function() body) =>
    HttpOverrides.runWithHttpOverrides(body, _PassthroughHttpOverrides());

void main() {
  // The capture happens in setUpAll, not inside the test body: `testWidgets`
  // runs under FakeAsync, where a real socket's futures never complete and the
  // test would simply hang.
  var up = false;
  var deltas = <String>[];
  var facts = <MemoryFact>[];

  setUpAll(() async {
    up = await _daemonUp();
    if (!up) return;

    await _withRealHttp(() async {
      final api = HttpCortexApi(baseUrl: _baseUrl);
      try {
        final sessions = await api.sessions();
        await for (final event in api.chat(
          sessionId: sessions.first.id,
          message: 'rust async trait',
        )) {
          if (event case ChatDeltaEvent(:final text)) deltas.add(text);
          if (event case ChatMemoryEvent(facts: final f)) facts = f;
        }
      } finally {
        api.dispose();
      }
    });
  });

  testWidgets('a live reply renders as markdown with a highlighted Rust block, '
      'and deltas extend the widget instead of replacing it', (tester) async {
    if (!up) return markTestSkipped('cortexd not running');

    tester.view.physicalSize = const Size(1200, 1400);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    expect(deltas.length, greaterThan(5), reason: 'must be a real stream');
    expect(facts, isNotEmpty);

    Widget frame(String text, bool streaming) => MaterialApp(
      theme: CortexTheme.dark(),
      home: Scaffold(
        body: SingleChildScrollView(
          child: AssistantBlock(
            text: text,
            facts: facts,
            toolCalls: const [],
            streaming: streaming,
          ),
        ),
      ),
    );

    // Replay the stream chunk by chunk, exactly as the controller would.
    var shown = '';
    Element? codeBlockElement;
    var sawOpenFence = false;

    for (final delta in deltas) {
      shown += delta;
      await tester.pumpWidget(frame(shown, true));
      await tester.pump(const Duration(milliseconds: 16));

      final found = find.byType(CodeBlock);
      if (found.evaluate().isNotEmpty) {
        final block = tester.widget<CodeBlock>(found);
        if (!block.closed) sawOpenFence = true;

        // Element identity must persist across deltas — that is what makes the
        // bubble grow in place rather than be rebuilt from scratch (the visible
        // symptom of which would be flicker and a scroll jump).
        codeBlockElement ??= found.evaluate().single;
        expect(
          found.evaluate().single,
          same(codeBlockElement),
          reason: 'the code block must be updated, not recreated',
        );
      }
    }

    // The unterminated fence was rendered as a code block while still open —
    // it never appeared as literal backticks that later flipped.
    expect(sawOpenFence, isTrue);

    await tester.pumpWidget(frame(shown, false));
    await tester.pump(const Duration(milliseconds: 50));

    final block = tester.widget<CodeBlock>(find.byType(CodeBlock));
    expect(block.language, 'rust');
    expect(block.closed, isTrue);
    expect(block.code, contains('println!'));

    // Genuinely tokenised: walk the rendered span tree and confirm the code is
    // painted in more than one colour. Checking the top-level child count would
    // not work — `Text.rich` wraps the supplied span in a single-child root.
    final codeSpan = find
        .descendant(of: find.byType(CodeBlock), matching: find.byType(RichText))
        .evaluate()
        .map((e) => (e.widget as RichText).text)
        .firstWhere((s) => s.toPlainText().contains('println!'));

    // `visitChildren` already descends the whole tree — recursing again inside
    // the visitor overflows the stack.
    final colours = <Color>{};
    codeSpan.visitChildren((span) {
      final colour = span.style?.color;
      if (colour != null) colours.add(colour);
      return true;
    });
    expect(
      colours.length,
      greaterThan(1),
      reason: 'Rust must render in multiple token colours, got $colours',
    );

    // Per-turn memory audit is attached with the live facts.
    expect(find.byType(MemoryDrawer), findsOneWidget);
    expect(find.textContaining('本轮用到的记忆'), findsOneWidget);

    // Expanding it reveals the injected statements and their provenance.
    await tester.tap(find.textContaining('本轮用到的记忆'));
    await tester.pump(const Duration(milliseconds: 250));
    expect(find.text(facts.first.statement), findsOneWidget);
    expect(find.text('出处'), findsWidgets);
  });
}
