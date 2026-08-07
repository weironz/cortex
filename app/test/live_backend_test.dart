@Tags(['live'])
library;

import 'dart:io';

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/sync_event.dart';
import 'package:cortex_app/models/tool_call.dart';
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

  /// Note what this test does **not** assert: that a `memory` event arrives.
  ///
  /// The live retriever abstains when nothing stored is relevant, so a fixed
  /// `['memory', 'tool', 'delta', 'done']` sequence is not a property of the
  /// protocol — it was a property of the old mock backend. Pinning it would
  /// turn correct server behaviour into a red test.
  test('POST /chat：增量只增不减，工具事件成对，done 收尾', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final sessions = await api.sessions();
    final events = <ChatEvent>[];
    var accumulated = '';
    var deltaCount = 0;
    var toolCalls = <ToolCall>[];

    await for (final event in api.chat(
      sessionId: sessions.first.id,
      // Phrased to make a tool call near-certain: the model cannot answer this
      // without reading the file, which is what puts a real call/result pair on
      // the wire.
      message: '读一下 app/pubspec.yaml，只回答 flutter sdk 的版本约束是什么，不要写代码块',
    )) {
      events.add(event);
      switch (event) {
        case ChatMemoryEvent(:final facts):
          for (final fact in facts) {
            expect(
              fact.sourceEpisodeId,
              isNotNull,
              reason: '注入的每条记忆都必须能追回原始对话，否则「可审计」是空话',
            );
          }
        case ChatToolEvent(:final name, :final summary):
          toolCalls = ToolCall.merge(toolCalls, name, summary);
        case ChatDeltaEvent(:final text):
          deltaCount++;
          // The invariant that makes the UI flicker-free: each delta EXTENDS
          // the previous text; it never replaces or rewrites it.
          final next = accumulated + text;
          expect(next.startsWith(accumulated), isTrue);
          accumulated = next;
        case ChatDoneEvent():
          break;
        case ChatErrorEvent(:final message):
          fail('server error: $message');
        case ChatUnknownEvent(:final type):
          fail('未知事件类型 $type —— 契约变了但客户端没跟上');
      }
    }

    // Genuinely incremental, not one buffered blob.
    expect(deltaCount, greaterThan(5));
    expect(accumulated, isNotEmpty);

    final done = events.whereType<ChatDoneEvent>().single;
    expect(done.episodeId, isNotNull, reason: 'done 必须带回 episode id 供追溯');

    final rawToolEvents = events.whereType<ChatToolEvent>().length;
    expect(toolCalls, isNotEmpty, reason: '这个问题必须触发一次 read_file');
    expect(
      rawToolEvents,
      toolCalls.length * 2,
      reason: '线上每次调用发两条事件（调用 + 返回），配对后行数应正好减半',
    );
    for (final call in toolCalls) {
      expect(call.pending, isFalse, reason: '${call.name} 的返回事件没有配上');
      expect(call.arguments, isNotNull, reason: '${call.name} 的参数摘要丢了');
      expect(call.result, isNotNull);
    }
  });

  test('GET /ws 只推信号，补拉一律用客户端自己的游标', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final events = <SyncEvent>[];
    final subscription = api.watchSync().listen(events.add);
    addTearDown(subscription.cancel);

    await _until(() => events.isNotEmpty, 'hello');
    final hello = events.first;
    expect(hello, isA<SyncHello>());
    expect(hello.cursor, greaterThan(0), reason: '库里已有数据，游标不该是 0');

    // Provoke a real commit. `/chat` writes the user episode *before* it calls
    // the model, so cancelling right after the first frame is enough to get a
    // bump without paying for a whole completion.
    final sessions = await api.sessions();
    final chat = api
        .chat(sessionId: sessions.first.id, message: 'ping：只是为了产生一次写入')
        .listen((_) {});
    await Future<void>.delayed(const Duration(seconds: 3));
    await chat.cancel();

    await _until(() => events.length > 1, 'bump');
    final signal = events[1];
    expect(
      signal,
      anyOf(isA<SyncBump>(), isA<SyncResync>()),
      reason: 'hello 之后应当是 bump（或 resync）',
    );
    expect(signal.cursor, greaterThan(hello.cursor));

    // The whole point: pull from **our** cursor, not the one the event carried.
    final page = await api.sync(since: hello.cursor);
    expect(page.records, isNotEmpty);
    expect(
      page.records.first.seq,
      hello.cursor + 1,
      reason: '第一条必须紧接自己的游标，中间不能有洞',
    );
    expect(page.records.any((r) => r.table == 'episodes'), isTrue);

    // And the counterfactual, so the rule is not merely asserted but shown:
    // starting from the event's cursor would have skipped those rows.
    final skipped = await api.sync(since: signal.cursor);
    expect(
      skipped.records.length,
      lessThan(page.records.length),
      reason: '拿事件里的 cursor 当 since 会漏掉 ${hello.cursor}..${signal.cursor}',
    );
  });

  test('rust 语法在半截围栏与完整代码上都能高亮', () async {
    // Deterministic on purpose. This used to be asserted against whatever the
    // model happened to emit, which made a highlighter regression and a chatty
    // model indistinguishable.
    const partial = 'fn hello() {\n    println!("cor';
    expect(
      HighlightRegistry.highlight(
        code: partial,
        language: 'rust',
        baseStyle: const TextStyle(),
        brightness: Brightness.dark,
      ),
      isNotNull,
      reason: '收尾围栏还没到时不能抛异常',
    );

    const complete = 'fn hello() {\n    println!("cortex");\n}\n';
    final span = HighlightRegistry.highlight(
      code: complete,
      language: 'rust',
      baseStyle: const TextStyle(),
      brightness: Brightness.dark,
    );
    expect(span, isNotNull, reason: 'rust 语法必须已注册');
    expect(
      span!.children,
      isNotNull,
      reason: '高亮结果必须是多个 span —— 单个 span 说明降级成纯文本了',
    );
    expect(span.children!.length, greaterThan(1));
  });
}

Future<void> _until(bool Function() condition, String what) async {
  final deadline = DateTime.now().add(const Duration(seconds: 20));
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待 $what 超时');
    await Future<void>.delayed(const Duration(milliseconds: 50));
  }
}
