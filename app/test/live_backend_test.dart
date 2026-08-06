@Tags(['live'])
library;

import 'dart:io';

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/widgets/markdown/highlight_registry.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// End-to-end checks against a running `cortexd`.
///
///   cd .. && cargo run -p cortexd
///   flutter test test/live_backend_test.dart
///
/// Skipped automatically when the daemon is not listening, so `flutter test`
/// stays green on a machine without a backend.
///
/// This file must contain **no** `testWidgets`: initialising
/// `TestWidgetsFlutterBinding` installs a global `HttpOverrides` that answers
/// every request with 400, for the whole suite. The rendering counterpart lives
/// in `live_render_test.dart`, which re-enables real HTTP inside a zone.
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

void main() {
  late bool up;

  setUpAll(() async => up = await _daemonUp());

  test('GET /health', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final health = await api.health();
    expect(health.status, 'ok');
    expect(health.isHealthy, isTrue, reason: 'database=not_wired must not fail');
  });

  test('GET /sessions', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final sessions = await api.sessions();
    expect(sessions, isNotEmpty);
    expect(sessions.first.id, isNotEmpty);
    expect(sessions.first.title, isNotEmpty);
  });

  test('GET /memory/search returns facts with retrieval channels', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final result = await api.searchMemory('Flutter', limit: 5);
    expect(result.facts, isNotEmpty);

    final fact = result.facts.first;
    expect(fact.sourceEpisodeId, isNotNull, reason: 'provenance is mandatory');
    expect(fact.createdAt, isNotNull);

    final channels = result.channels[fact.id];
    expect(channels, isNotNull, reason: 'channel attribution must decode');
    expect(channels!.channels, isNotEmpty);
  });

  test('as_of replays transaction time', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final now = await api.searchMemory('Flutter', limit: 5);
    expect(now.facts, isNotEmpty);

    // Anchor strictly before the fact was recorded; it must disappear.
    final before = now.facts.first.createdAt!.subtract(
      const Duration(days: 2),
    );
    final past = await api.searchMemory('Flutter', limit: 5, asOf: before);

    expect(
      past.facts.map((f) => f.id),
      isNot(contains(now.facts.first.id)),
      reason: 'a fact must not exist before Cortex learned it',
    );
  });

  test('GET /episodes/{id} resolves a fact back to its source', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final result = await api.searchMemory('Flutter', limit: 1);
    final episodeId = result.facts.first.sourceEpisodeId!;

    final episode = await api.episode(episodeId);
    expect(episode.id, episodeId);
    expect(episode.text, isNotEmpty);
  });

  test('POST /chat streams memory -> tool -> delta* -> done', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final sessions = await api.sessions();
    final events = <ChatEvent>[];
    final arrivalOrder = <String>[];
    var accumulated = '';
    final snapshots = <String>[];

    await for (final event in api.chat(
      sessionId: sessions.first.id,
      message: 'rust async trait',
    )) {
      events.add(event);
      switch (event) {
        case ChatMemoryEvent():
          arrivalOrder.add('memory');
        case ChatToolEvent():
          arrivalOrder.add('tool');
        case ChatDeltaEvent(:final text):
          if (arrivalOrder.isEmpty || arrivalOrder.last != 'delta') {
            arrivalOrder.add('delta');
          }
          // The invariant that makes the UI flicker-free: each delta EXTENDS
          // the previous text; it never replaces or rewrites it.
          final next = accumulated + text;
          expect(next.startsWith(accumulated), isTrue);
          accumulated = next;
          snapshots.add(accumulated);
        case ChatDoneEvent():
          arrivalOrder.add('done');
        case ChatErrorEvent(:final message):
          fail('server error: $message');
        case ChatUnknownEvent(:final type):
          arrivalOrder.add('unknown:$type');
      }
    }

    expect(arrivalOrder, ['memory', 'tool', 'delta', 'done']);

    // Genuinely incremental, not one buffered blob.
    expect(snapshots.length, greaterThan(5));
    expect(accumulated, isNotEmpty);

    final memory = events.whereType<ChatMemoryEvent>().single;
    expect(memory.facts, isNotEmpty);
    expect(memory.facts.first.sourceEpisodeId, isNotNull);

    final done = events.whereType<ChatDoneEvent>().single;
    expect(done.episodeId, isNotNull);

    // The reply contains a Rust fence; confirm the highlighter actually
    // tokenises it rather than silently falling back to plain text.
    expect(accumulated, contains('```rust'));
    final code = RegExp(
      r'```rust\n([\s\S]*?)```',
    ).firstMatch(accumulated)?.group(1);
    expect(code, isNotNull);

    final span = HighlightRegistry.highlight(
      code: code!,
      language: 'rust',
      baseStyle: const TextStyle(),
      brightness: Brightness.dark,
    );
    expect(span, isNotNull, reason: 'rust grammar must be registered');
    expect(
      span!.children,
      isNotNull,
      reason: 'highlighted output must be multi-span, i.e. actually coloured',
    );
    expect(span.children!.length, greaterThan(1));
  });

  test('a half-arrived rust fence still resolves to the rust grammar', () async {
    // Mid-stream the closing fence has not landed; the code block widget is
    // handed the partial body and must not throw.
    const partial = 'fn hello() {\n    println!("cor';
    final span = HighlightRegistry.highlight(
      code: partial,
      language: 'rust',
      baseStyle: const TextStyle(),
      brightness: Brightness.dark,
    );
    expect(span, isNotNull);
  });
}
