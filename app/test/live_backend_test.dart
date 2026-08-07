@Tags(['live'])
library;

import 'dart:io';
import 'dart:typed_data';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/blob_upload.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/hashing.dart';
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

  // ─────────────────── 会话生命周期与工作区 ───────────────────

  test('GET /sessions/{id} 回放整段会话，附件字段一定在', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final sessions = await api.sessions();
    final detail = await api.sessionDetail(sessions.first.id);

    expect(detail.session.id, sessions.first.id);
    expect(detail.episodes, isNotEmpty, reason: '列表里的会话至少有一条消息');
    for (final e in detail.episodes) {
      // The server sends `[]` rather than omitting the key, so the client never
      // has to distinguish "no attachments" from "server too old to report".
      expect(e.attachments, isNotNull);
      expect(e.id, isNotEmpty);
    }
    expect(
      detail.episodes.length,
      lessThanOrEqualTo(500),
      reason: 'SESSION_EPISODE_LIMIT；超过就该被 truncated 标出来',
    );
    expect(
      detail.truncated,
      detail.session.messageCount > detail.episodes.length,
      reason: '截断判定必须与 message_count 一致，否则界面会谎报完整',
    );
  });

  test('PATCH /sessions/{id} 改名并回写 title_is_custom', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final target = (await api.sessions()).first;
    final restore = target.titleIsCustom ? target.title : null;

    final renamed = await api.updateSession(
      target.id,
      title: '联调改名 ${DateTime.now().millisecondsSinceEpoch}',
    );
    expect(renamed.titleIsCustom, isTrue);
    expect(renamed.title, startsWith('联调改名'));

    if (restore != null) await api.updateSession(target.id, title: restore);
  });

  test('归档与取消归档，include_archived 决定它出不出现在列表里', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final target = (await api.sessions()).first;
    addTearDown(() => api.updateSession(target.id, archived: false));

    await api.updateSession(target.id, archived: true);

    expect(
      (await api.sessions()).any((s) => s.id == target.id),
      isFalse,
      reason: '默认列表不含已归档',
    );

    final found = (await api.sessions(
      includeArchived: true,
    )).firstWhere((s) => s.id == target.id);
    expect(found.archived, isTrue);
    expect(
      found.messageCount,
      greaterThan(0),
      reason: '归档不是删除 —— 消息一条没少',
    );
  });

  test('工作区三态：绑定 / 不动 / 解绑', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final target = (await api.sessions()).first;
    addTearDown(() => api.updateSession(target.id, clearWorkspace: true));

    final bound = await api.updateSession(
      target.id,
      workspace: Directory.current.path,
    );
    expect(bound.workspace, isNotNull);

    // Field absent = leave it alone. This is the state a plain `Option<String>`
    // on the server would collapse into "unbind".
    final untouched = await api.updateSession(target.id, title: bound.title);
    expect(
      untouched.workspace?.root,
      bound.workspace?.root,
      reason: '不传 workspace 就该原样保留',
    );

    final unbound = await api.updateSession(target.id, clearWorkspace: true);
    expect(
      unbound.workspace,
      isNull,
      reason: '显式 null 才是解绑；两态混起来会让会话莫名其妙失去文件工具',
    );
  });

  test('非法工作区路径带回可直接展示给用户的理由', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final target = (await api.sessions()).first;

    await expectLater(
      api.updateSession(target.id, workspace: 'relative/path'),
      throwsA(
        isA<CortexApiException>()
            .having((e) => e.statusCode, 'statusCode', 400)
            .having((e) => e.message, 'message', contains('绝对路径'))
            // The client must unwrap `ErrorBody { error }`. Showing raw JSON
            // would throw away wording that was written to be read.
            .having((e) => e.message, 'message', isNot(contains('{"error"'))),
      ),
    );

    await expectLater(
      api.updateSession(target.id, workspace: '/definitely/not/here/xyzzy'),
      throwsA(
        isA<CortexApiException>().having(
          (e) => e.isUnsupported,
          'isUnsupported',
          isFalse,
        ),
      ),
    );
  });

  // ───────────────────────────── 附件 ─────────────────────────────

  test('POST /blobs 中转上传，哈希由内容决定且可原样取回', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final bytes = _tinyPng();
    final progress = <int>[];
    final attachment = await uploadAttachment(
      api,
      bytes: bytes,
      filename: 'live-test.png',
      onProgress: (sent, _) => progress.add(sent),
    );

    expect(
      attachment.hash,
      await sha256Hex(bytes),
      reason: '两端算的哈希必须一致，否则 presign 那条路根本对不上 key',
    );
    expect(
      attachment.mime,
      'image/png',
      reason: 'MIME 由字节头嗅探得出，不是客户端声明的那个',
    );
    expect(attachment.kind, 'image');
    expect(progress, isNotEmpty);
    expect(progress.last, bytes.length);

    expect(
      await api.blobBytes(attachment.hash),
      equals(bytes),
      reason: '取回的字节必须与上传的一模一样',
    );
  });

  test('presign 对已存在的内容直接说 already_uploaded', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final bytes = _tinyPng();
    await api.uploadBlob(bytes: bytes, mime: 'image/png');
    final hash = await sha256Hex(bytes);

    try {
      final presign = await api.presignBlob(hash);
      expect(presign.method, 'PUT');
      expect(presign.url, contains(hash));
      expect(
        presign.alreadyUploaded,
        isTrue,
        reason: '刚传过的内容不该被要求再传一遍 —— 这是内容寻址省下的那笔带宽',
      );
    } on CortexApiException catch (e) {
      // A local_fs deployment answers 501 by design. That is a valid outcome;
      // the client's job is to recognise it and not retry.
      expect(
        e.isUnsupported,
        isTrue,
        reason: '签不出 URL 只应表现为 501（本部署不支持），而不是别的失败',
      );
    }
  });

  test('带附件的一轮对话：blob 先登记，再由 /chat 关联到 episode', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final attachment = await uploadAttachment(
      api,
      bytes: _tinyPng(),
      filename: 'live-attach.png',
    );

    final sessionId = 'live-attach-${DateTime.now().millisecondsSinceEpoch}';
    var done = false;
    await for (final event in api.chat(
      sessionId: sessionId,
      message: '这是一张测试图，收到请回复「收到」。',
      attachments: [attachment],
    )) {
      if (event is ChatErrorEvent) fail('服务端拒绝了这一轮：${event.message}');
      if (event is ChatDoneEvent) done = true;
    }
    expect(done, isTrue, reason: '未登记的 hash 会让整轮被拒，这里必须走通');

    final detail = await api.sessionDetail(sessionId);
    final userTurn = detail.episodes.firstWhere((e) => e.role == 'user');
    expect(
      userTurn.attachments.map((a) => a.hash),
      contains(attachment.hash),
      reason: '附件必须挂在 episode 上，回放时才看得见',
    );
    expect(
      userTurn.attachments.first.filename,
      isNull,
      reason: 'AttachmentRef 只有 hash 与 kind —— 文件名不过夜，这是已知的契约缺口',
    );
  });

  test('绑定工作区后，文件工具的摘要能被解析出路径', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final sessionId = 'live-ws-${DateTime.now().millisecondsSinceEpoch}';
    // The session row only exists once it has a turn, so bind after the first
    // message rather than before it.
    await api.chat(sessionId: sessionId, message: '你好').drain<void>();
    await api.updateSession(sessionId, workspace: Directory.current.path);

    var calls = <ToolCall>[];
    await for (final event in api.chat(
      sessionId: sessionId,
      message: '用 list_dir 列一下工作区根目录，再用 read_file 读 pubspec.yaml。',
    )) {
      if (event is ChatToolEvent) {
        calls = ToolCall.merge(calls, event.name, event.summary);
      }
    }

    final fileCalls = calls.where((c) => c.touchesFiles).toList();
    expect(
      fileCalls,
      isNotEmpty,
      reason: '绑定了工作区，文件工具就该出现在工具目录里',
    );
    for (final call in fileCalls) {
      expect(
        call.targetPath,
        isNotNull,
        reason:
            '${call.name} 的摘要里应能解析出 path —— 界面靠它说明动了哪个文件：'
            '${call.arguments}',
      );
      expect(call.pending, isFalse, reason: '两条事件应配成一行');
    }
  });

  test('未绑定工作区的会话拿不到文件工具', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = HttpCortexApi(baseUrl: _baseUrl);
    addTearDown(api.dispose);

    final sessionId = 'live-nows-${DateTime.now().millisecondsSinceEpoch}';
    var calls = <ToolCall>[];
    await for (final event in api.chat(
      sessionId: sessionId,
      message: '读一下工作区里的 pubspec.yaml，告诉我 name 字段。',
    )) {
      if (event is ChatToolEvent) {
        calls = ToolCall.merge(calls, event.name, event.summary);
      }
    }

    expect(
      calls.where((c) => c.touchesFiles),
      isEmpty,
      reason:
          '纯聊天会话的工具目录里根本没有文件工具（WORKSPACE_FREE_TOOLS），'
          '这正是「绑定与否」在产品上唯一的差别',
    );
  });
}

/// A 2×2 PNG — small enough to upload in a test, real enough for the server to
/// sniff `image/png` out of its header rather than trusting the declared type.
Uint8List _tinyPng() => Uint8List.fromList(const [
  0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, //
  0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
  0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02,
  0x08, 0x02, 0x00, 0x00, 0x00, 0xFD, 0xD4, 0x9A,
  0x73, 0x00, 0x00, 0x00, 0x16, 0x49, 0x44, 0x41,
  0x54, 0x78, 0x9C, 0x63, 0xFC, 0xCF, 0xC0, 0xF0,
  0x9F, 0x01, 0x13, 0xFF, 0x19, 0x18, 0x00, 0x00,
  0x2A, 0x0B, 0x03, 0x01, 0x6D, 0x0E, 0x2C, 0x8B,
  0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
  0xAE, 0x42, 0x60, 0x82,
]);

Future<void> _until(bool Function() condition, String what) async {
  final deadline = DateTime.now().add(const Duration(seconds: 20));
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待 $what 超时');
    await Future<void>.delayed(const Duration(milliseconds: 50));
  }
}
