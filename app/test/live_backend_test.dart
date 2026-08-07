@Tags(['live'])
library;

import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/blob_upload.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/hashing.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/models/session_detail.dart';
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

/// The credential every case here connects with.
///
/// Read from the environment rather than hard-coded, because a real `cortexd`
/// **refuses to start** without credentials configured — so a suite that
/// connected anonymously would 401 on every case against any daemon anyone
/// actually runs. This is the same variable `cortexd --generate-token` prints.
///
///     CORTEXD_TOKEN=<明文> flutter test test/live_backend_test.dart
///
/// Null against a daemon started with `CORTEX_AUTH=disabled`, where it is not
/// needed and sending one would be noise.
final String? _token = () {
  final raw = Platform.environment['CORTEXD_TOKEN']?.trim();
  return (raw == null || raw.isEmpty) ? null : raw;
}();

HttpCortexApi _api({String? token, void Function()? onUnauthorized}) =>
    HttpCortexApi(
      baseUrl: _baseUrl,
      token: token ?? _token,
      onUnauthorized: onUnauthorized,
    );

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
    final api = _api();
    addTearDown(api.dispose);

    final health = await api.health();
    expect(health.status, 'ok');
    expect(health.isHealthy, isTrue, reason: 'database=not_wired must not fail');
  });

  test('GET /sessions', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    final sessions = await api.sessions();
    expect(sessions, isNotEmpty);
    expect(sessions.first.id, isNotEmpty);
    expect(sessions.first.title, isNotEmpty);
  });

  test('GET /memory/search returns facts with retrieval channels', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
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
    final api = _api();
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
    final api = _api();
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
    final api = _api();
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
        case ChatToolEvent(:final name, :final summary, :final path):
          toolCalls = ToolCall.merge(toolCalls, name, summary, path: path);
        case ChatDeltaEvent(:final text):
          deltaCount++;
          // The invariant that makes the UI flicker-free: each delta EXTENDS
          // the previous text; it never replaces or rewrites it.
          final next = accumulated + text;
          expect(next.startsWith(accumulated), isTrue);
          accumulated = next;
        case ChatConfirmEvent(:final request):
          // This prompt asks to read a file, so no `Risk::Execute` tool should
          // be reached. If one is, the turn is now suspended waiting for a
          // receipt that this test will never send — it would hang until the
          // daemon's timeout. Failing immediately says what happened.
          fail(
            '这一轮不该触发高风险确认，但收到了 ${request.tool}（${request.risk}）：'
            '${request.preview}',
          );
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
    final api = _api();
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

  test('GET /sessions/{id} 默认给最新一页，附件带元信息', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    final sessions = await api.sessions();
    final detail = await api.sessionDetail(sessions.first.id, limit: 5);

    expect(detail.session.id, sessions.first.id);
    expect(detail.episodes, isNotEmpty, reason: '列表里的会话至少有一条消息');
    expect(
      detail.episodes.length,
      lessThanOrEqualTo(5),
      reason: 'limit 是客户端说了算的上界，服务端只能给得更少',
    );
    for (final e in detail.episodes) {
      // The server sends `[]` rather than omitting the key, so the client never
      // has to distinguish "no attachments" from "server too old to report".
      expect(e.attachments, isNotNull);
      expect(e.id, isNotEmpty);
      for (final a in e.attachments) {
        expect(a.mime, isNotNull, reason: '下行的 AttachmentDto 必须带嗅探出的 MIME');
        expect(a.sizeBytes, isNotNull);
      }
    }
    // Ascending within the page — a descending page would not error anywhere,
    // it would just draw the whole conversation backwards.
    for (var i = 1; i < detail.episodes.length; i++) {
      final prev = detail.episodes[i - 1].occurredAt;
      final cur = detail.episodes[i].occurredAt;
      if (prev == null || cur == null) continue;
      expect(
        cur.isBefore(prev),
        isFalse,
        reason: '一页之内必须是正序（老 → 新）',
      );
    }
    expect(
      detail.nextCursor != null,
      detail.hasMore,
      reason: 'has_more 为真才有游标，为假就必须是 null',
    );
  });

  test('?before= 往回翻，翻出来的严格更早且不重复', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    // 找一个至少有两页（这里 1 条一页）的会话
    final sessions = await api.sessions();
    SessionDetail? first;
    for (final s in sessions) {
      final d = await api.sessionDetail(s.id, limit: 1);
      if (d.hasMore) {
        first = d;
        break;
      }
    }
    if (first == null) {
      return markTestSkipped('没有超过一条消息的会话可供翻页');
    }

    final second = await api.sessionDetail(
      first.session.id,
      limit: 1,
      before: first.nextCursor,
    );
    expect(second.episodes, isNotEmpty, reason: 'has_more 为真就必须真的翻得到');
    expect(
      second.episodes.map((e) => e.id),
      isNot(contains(first.episodes.first.id)),
      reason: 'before 是严格小于 —— 含等号的话每页都会重复一条',
    );
    final older = second.episodes.last.occurredAt;
    final newer = first.episodes.first.occurredAt;
    if (older != null && newer != null) {
      expect(older.isAfter(newer), isFalse, reason: '往回翻必须真的更早');
    }
  });

  test('畸形游标是 400，不是 500', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    final sessions = await api.sessions();
    await expectLater(
      api.sessionDetail(sessions.first.id, before: '昨天|不是个ULID'),
      throwsA(
        isA<CortexApiException>().having(
          (e) => e.statusCode,
          'statusCode',
          400,
        ),
      ),
      reason: '游标进 SQL 之前就该被拦下 —— 撞上 ulid 域约束的话客户端只会看到「服务端挂了」',
    );
  });

  test('PATCH /sessions/{id} 改名并回写 title_is_custom', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
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
    final api = _api();
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
    final api = _api();
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
    final api = _api();
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
    final api = _api();
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
    final api = _api();
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
    final api = _api();
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
    final replayed = userTurn.attachments.first;
    expect(
      replayed.filename,
      'live-attach.png',
      reason:
          '文件名现在往返了：AttachmentRef 收 filename，AttachmentDto 回带它。'
          '拿到 null 就说明客户端又没把它发上去，回放会退回「文档 · a1b2c3d4」',
    );
    expect(
      replayed.mime,
      'image/png',
      reason: 'MIME 由字节头嗅探，不是客户端声明的那个',
    );
    expect(replayed.sizeBytes, greaterThan(0));
  });

  test('绑定工作区后，文件工具事件带回结构化的 path', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
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
        calls = ToolCall.merge(
          calls,
          event.name,
          event.summary,
          path: event.path,
        );
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
        call.path,
        isNotNull,
        reason:
            '${call.name} 必须自带 path —— 界面靠它说明动了哪个文件，'
            '从摘要里正则抠出来的那套已经删掉了：${call.arguments}',
      );
      expect(call.pending, isFalse, reason: '两条事件应配成一行');
    }
  });

  test('未绑定工作区的会话拿不到文件工具', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    final sessionId = 'live-nows-${DateTime.now().millisecondsSinceEpoch}';
    var calls = <ToolCall>[];
    await for (final event in api.chat(
      sessionId: sessionId,
      message: '读一下工作区里的 pubspec.yaml，告诉我 name 字段。',
    )) {
      if (event is ChatToolEvent) {
        calls = ToolCall.merge(
          calls,
          event.name,
          event.summary,
          path: event.path,
        );
      }
    }

    // 断言的是「一次文件操作都没有真的发生」，而不是「没有出现过文件工具名」。
    // 模型完全可以凭空调一个不存在的工具（实测见过 `file_read`），而 daemon
    // 照样会把这次尝试作为 tool 事件发出来 —— 带着它从参数里取到的 path。
    // 那条事件是**失败**的（「未知工具：…。可用工具：memory_search」），
    // 界面也按失败画。把「名字里带 read_file」当成红线，测的就成了模型今天
    // 想怎么命名，而不是服务端到底给了什么工具。
    final succeededFileCalls = calls
        .where((c) => c.touchesFiles && !c.failed)
        .toList();
    expect(
      succeededFileCalls,
      isEmpty,
      reason:
          '纯聊天会话的工具目录里根本没有文件工具（WORKSPACE_FREE_TOOLS），'
          '这正是「绑定与否」在产品上唯一的差别；'
          '实际拿到：${calls.map((c) => "${c.name}/${c.result}").toList()}',
    );
  });

  // ───────────────────────────── 认证 ─────────────────────────────

  test('GET /health 不需要认证，并说明本部署到底开没开', () async {
    if (!up) return markTestSkipped('cortexd not running');
    // 刻意不带任何凭据：这是唯一一条免认证的路由，而一个还没拿到 token 的
    // 客户端只能靠它回答「我要不要去找一个 token」
    final api = _api(token: '');
    addTearDown(api.dispose);

    final health = await api.health();
    expect(
      health.auth,
      anyOf('token', 'disabled'),
      reason: '没有这一行，「我到底有没有被保护」就只能去翻服务器上的环境变量',
    );
    expect(health.auth, isNot(HealthStatus.authUnknown));
  });

  test('没有凭据时受保护的路由回 401，且客户端能自愈', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final probe = _api(token: '');
    addTearDown(probe.dispose);
    if ((await probe.health()).authDisabled) {
      return markTestSkipped('这个 daemon 关闭了认证，401 这条路走不到');
    }

    var fired = 0;
    final api = _api(token: 'not-the-token', onUnauthorized: () => fired++);
    addTearDown(api.dispose);

    await expectLater(
      api.sessions(),
      throwsA(
        isA<CortexApiException>().having(
          (e) => e.isUnauthorized,
          'isUnauthorized',
          isTrue,
        ),
      ),
    );
    await Future<void>.delayed(Duration.zero);
    expect(fired, 1, reason: '401 必须把客户端打回登录态，而不是留下一个空白面板');
  });

  test('POST /auth/ticket 换到的票据能开 WebSocket', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    final ticket = await api.issueTicket();
    expect(ticket.value, isNotEmpty);
    expect(
      ticket.isUsableAt(DateTime.now()),
      isTrue,
      reason: '刚签出来的票必须是可用的，否则 WS 永远连不上',
    );

    // 真连一次：`watchSync` 内部先换票再握手，这一条同时验证了
    // 「票据确实被服务端接受」与「浏览器加不了头这条路真的走得通」
    final first = await api.watchSync().first.timeout(
      const Duration(seconds: 10),
    );
    expect(first, isA<SyncHello>());
  });

  // ─────────────────────── 工具确认（R11）───────────────────────

  test('确认回路：请求自带预览，回执被接受，晚到的拿 404', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    final sessionId = 'live-confirm-${DateTime.now().millisecondsSinceEpoch}';
    ChatConfirmEvent? asked;
    final texts = StringBuffer();

    // `#confirm` 是 cortexd mock 后端的触发口令（`MOCK_CONFIRM_TRIGGER`）。
    // 接了真模型的部署上这一条会拿不到确认事件而跳过 —— 那也是对的：
    // 让一个真的 shell 命令挂起在 CI 上不是这条用例该做的事
    final stream = api.chat(sessionId: sessionId, message: '#confirm 试一下确认回路');
    late final StreamSubscription<ChatEvent> sub;
    sub = stream.listen((event) {
      if (event is ChatConfirmEvent) asked = event;
      if (event is ChatDeltaEvent) texts.write(event.text);
    });
    addTearDown(sub.cancel);

    // 等确认请求到达，最多几秒
    final deadline = DateTime.now().add(const Duration(seconds: 8));
    while (asked == null && DateTime.now().isBefore(deadline)) {
      await Future<void>.delayed(const Duration(milliseconds: 100));
    }
    final request = asked;
    if (request == null) {
      return markTestSkipped('这个部署没有触发确认（多半接的是真模型，不是 mock 后端）');
    }

    expect(request.request.tool, isNotEmpty);
    expect(
      request.request.risk,
      anyOf('execute', 'write'),
      reason: '风险等级要能被画成一个标签，未知值必须原样透出而不是降级成最轻的那个',
    );
    expect(
      request.request.preview,
      isNotEmpty,
      reason: '预览是用户据以判断的全部依据。「agent 想执行一个工具」这种话没有信息量',
    );
    expect(
      request.request.remainingFrom(DateTime.now()).inSeconds,
      greaterThan(0),
      reason: '倒计时得有个正数可画 —— 超时是静默的，用户必须知道自己还有多久',
    );

    // 待办列表里必须能看到它 —— 这就是断线重连后捞回来的那条路
    final pending = await api.pendingConfirmations(sessionId: sessionId);
    expect(
      pending.map((c) => c.token),
      contains(request.request.token),
      reason: 'SSE 断了不会重发；GET /confirmations 是重连后唯一的答案',
    );
    expect(
      pending.first.sessionId,
      sessionId,
      reason: '恢复端点带 session_id（SSE 事件不带），客户端据此知道该跳去哪个会话',
    );

    // 回执被接受
    expect(
      await api.answerConfirmation(token: request.request.token, allow: true),
      isTrue,
    );

    // 同一个 token 再投一次 —— 这正是「别的设备先答了」在晚到那一方看到的样子
    expect(
      await api.answerConfirmation(token: request.request.token, allow: true),
      isFalse,
      reason: '一次性凭据：先到的作数，晚到的拿 404。这是正常情况，不是错误',
    );

    // 答复真的解开了那一轮
    await _until(() => texts.isNotEmpty, '批准之后继续吐字');
  });

  test('GET /confirmations 在没有待办时是空列表而不是 404', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    expect(
      await api.pendingConfirmations(
        sessionId: 'no-such-session-${DateTime.now().microsecondsSinceEpoch}',
      ),
      isEmpty,
    );
  });

  test('伪造的回执 token 拿到 404，而不是 500 或静默成功', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    expect(
      await api.answerConfirmation(token: 'deadbeef' * 8, allow: true),
      isFalse,
      reason: '伪造的、已被答过的、超时的、那一轮早结束的 —— 服务端刻意不区分这四种',
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
