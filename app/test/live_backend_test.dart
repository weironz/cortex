@Tags(['live'])
library;

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

/// End-to-end checks against a running deployment.
///
///   just dev
///   flutter test test/live_backend_test.dart
///
/// # 打的是**边缘**那个口，不是任何单个服务
///
/// 从前这里写的是 `cortexd` 的 :8080。容器编排搬去 `cortex-agentd` 之后，
/// `/chat` 与 `/sandbox/*` 归它（:8081），其余仍归 cortexd —— 也就是说
/// **没有任何一个服务能独自应答这一整套用例了**。
///
/// 指向 nginx（:5173）不是权宜：那才是客户端真正看到的形状。直接打某个
/// 服务的端口验的是一条生产上不存在的拓扑，而分流本身（哪条路归谁）
/// 恰恰是这次拆分最容易配错的一环 —— 绕过它就等于不测它。
///
/// Skipped automatically when the deployment is not listening, so
/// `flutter test` stays green on a machine without a backend.
///
/// This file must contain **no** `testWidgets`: initialising
/// `TestWidgetsFlutterBinding` installs a global `HttpOverrides` that answers
/// every request with 400, for the whole suite. The rendering counterpart lives
/// in `live_render_test.dart`, which re-enables real HTTP inside a zone.
const _baseUrl = 'http://127.0.0.1:5173';

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
      5173,
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
    expect(
      health.isHealthy,
      isTrue,
      reason: 'database=not_wired must not fail',
    );
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

  // 三条记忆用例（/memory/search、as_of 回放、episode 溯源）随记忆界面一起
  // 去了 Cormex —— 那三条测的是**记忆服务的契约**，而这个客户端不再有任何
  // 记忆界面。它们在 Cormex 那个仓库里有对应的覆盖。
  //
  // 留在这儿只会变成一批「测一个我们不再使用的端点」的用例：它们红了
  // 说明记忆服务改了，而这一侧没有任何人需要知道。

  test('POST /chat：增量只增不减，工具事件成对，done 收尾', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    final events = <ChatEvent>[];
    var accumulated = '';
    var deltaCount = 0;
    var toolCalls = <ToolCall>[];

    await for (final event in api.chat(
      // **全新会话**，不是 `sessions.first`。整份文件跑下来，前面十几轮已经把
      // 答案留在了那条会话的历史里 —— 模型于是直接答，一次工具都不调，
      // 而这条用例要的正是那对「调用 + 返回」。单跑绿、整跑红，就是这个原因
      sessionId: 'live-chat-${DateTime.now().millisecondsSinceEpoch}',
      // 点名一个工具，好让那对事件必定上线。
      //
      // 从前这里点的是 `read_file`。cortexd 现在一个文件工具都不给
      //（`WORKSPACE_FREE_TOOLS` 只有 `memory_search`），继续点它测的就成了
      // 「模型面对一个不存在的工具会怎么办」—— 那是模型的事，不是契约的事
      message: '用 memory_search 工具查一下 Cortex 这个项目，然后用一句话说说你查到了什么。',
    )) {
      events.add(event);
      switch (event) {
        // 服务端仍然会发 `type: "memory"` —— 它落到 `ChatUnknownEvent`
        // 那条回落上，被安静忽略。**不为它写分支是有意的**：这一侧不再有
        // 记忆界面，接住它只会引出一个没人渲染的字段。
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
    expect(toolCalls, isNotEmpty, reason: '这个问题必须触发一次 memory_search');
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

  // 默认 30s 不够：两次 `_until` 各留 20s，中间还夹着 3s 的等待，
  // 于是框架的超时总是先到 —— 拿到的是一句光秃秃的 TimeoutException，
  // 而**卡在等 hello 还是等 bump** 恰恰是唯一有用的那点信息。
  // 给足预算，让 `_until` 自己的「等待 X 超时」先说话
  test(
    'GET /ws 只推信号，补拉一律用客户端自己的游标',
    timeout: const Timeout(Duration(seconds: 60)),
    () async {
      if (!up) return markTestSkipped('cortexd not running');
      final api = _api();
      addTearDown(api.dispose);

      final events = <SyncEvent>[];
      final subscription = api.watchSync().listen(events.add);
      // 同样套超时，理由同下面那条 `chat.cancel`：收尾里挂住的话，测试框架
      // 只会报「整条用例超时」，而**断言早就全过了** —— 那句话会把人送去查
      // 一个根本没坏的东西。断开实时同步这条路，应用里正是切后端 / 退登录时走的
      addTearDown(
        () => subscription.cancel().timeout(
          const Duration(seconds: 10),
          onTimeout: () => fail(
            '取消 watchSync 订阅挂住了 —— 应用里切后端、退登录都要先断开它，'
            '它挂住就是那两个动作卡死',
          ),
        ),
      );

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
      // 套一层超时，说清是**谁**没回来。裸 `await` 挂住时，测试框架只会给一句
      // 光秃秃的 TimeoutException —— 而「取消一条在飞的 SSE 订阅会不会挂」
      // 与「bump 到底来没来」是两个完全不同的结论，光看那句话分不出来
      await chat.cancel().timeout(
        const Duration(seconds: 10),
        onTimeout: () => fail(
          '取消在飞的 /chat 订阅挂住了 —— 界面上「停止生成」走的正是这条路，'
          '它挂住就是按钮按下去没反应',
        ),
      );

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
    },
  );

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
      expect(cur.isBefore(prev), isFalse, reason: '一页之内必须是正序（老 → 新）');
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
    expect(found.messageCount, greaterThan(0), reason: '归档不是删除 —— 消息一条没少');
  });

  /// cortexd **不提供**文件执行环境：绑定要被明确拒绝，而拒绝里要写清去哪跑。
  ///
  /// # 这条测试的前身
  ///
  /// 它原本断言「绑定 / 不动 / 解绑」三态都能在 cortexd 上走通。那是文件工具
  /// 还长在服务端时的事 —— 那条路绑的是**服务器上**的一个目录，于是一个远端
  /// 用户的 `read_file` 动的是生产机的文件系统。工具搬回本机之后三态只剩两态，
  /// 而剩下那条「绑定」必须是拒绝。
  ///
  /// # 为什么盯着报错的**措辞**
  ///
  /// 这里唯一真正糟糕的结局不是报错，是静默忽略：客户端会显示
  /// 「已绑定 D:\myproject」，然后每个文件操作都失败得莫名其妙 —— 用户明明
  /// 看到绑定成功了。退一步，只说「不行」同样把人晾在原地：他手上有个目录、
  /// 有个要改的文件，需要知道的是**下一步去哪**。所以两条走得通的路
  ///（桌面端 / 本机跑 cortex-local）都必须出现在这一句里。
  test('cortexd 拒绝在服务端绑工作区，并说清该去哪跑', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    final target = (await api.sessions()).first;

    await expectLater(
      api.updateSession(target.id, workspace: Directory.current.path),
      throwsA(
        isA<CortexApiException>()
            .having((e) => e.statusCode, 'statusCode', 400)
            // 400 而不是 501：这不是「本部署没开这个功能」，是「这件事在服务端
            // 永远不成立」。`isUnsupported` 会把 501 读成前者并把整块界面收起来
            .having((e) => e.isUnsupported, 'isUnsupported', isFalse)
            .having(
              (e) => e.message,
              'message',
              allOf(contains('桌面端'), contains('cortex-local')),
            )
            // The client must unwrap `ErrorBody { error }`. Showing raw JSON
            // would throw away wording that was written to be read.
            .having((e) => e.message, 'message', isNot(contains('{"error"'))),
      ),
    );

    // 解绑**照旧放行**：老会话上可能还留着一条服务端绑定（这次改动之前存下的）。
    // 一起拒掉等于把那条记录永远焊在那儿，而用户唯一能做的就是删掉整个会话
    final unbound = await api.updateSession(target.id, clearWorkspace: true);
    expect(
      unbound.workspace,
      isNull,
      reason: '解绑不该被「服务端不给绑」这条规则连坐 —— 它是清理，不是绑定',
    );

    // 不传这个字段 = 不动它。这正是服务端上一个裸 `Option<String>` 会塌成
    // 「解绑」的那一态；这里顺带确认它没塌
    final untouched = await api.updateSession(target.id, title: target.title);
    expect(untouched.workspace, isNull);
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
    expect(attachment.mime, 'image/png', reason: 'MIME 由字节头嗅探得出，不是客户端声明的那个');
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
    expect(replayed.mime, 'image/png', reason: 'MIME 由字节头嗅探，不是客户端声明的那个');
    expect(replayed.sizeBytes, greaterThan(0));
  });

  // 这里原本还有一条「绑定工作区后，文件工具事件带回结构化的 path」。
  // 它在 cortexd 上已经**无法成立** —— 绑不上工作区（见上面那条拒绝测试）。
  // 删掉而不是留着看它红：它测的「`path` 是独立一列、不从 summary 里正则抠」
  // 现在钉在两个够得着的地方 —— `cortex-store` 的
  // `tool_calls_replay_with_a_separate_path_column`（落库那半）与本机 agent
  // 自己的用例（在线那半）。
  //
  // 这里也曾有一条「cortexd 上的会话一律拿不到文件工具」。**它的前提在沙箱
  // 透明化（任务 #105）那天就没了**：云端会话现在按需拉起一个容器，每一个都
  // 有全套文件工具，「纯聊天会话」在 cortexd 上不再是一种状态。
  //
  // 换成钉它的**反面**，也就是 #105 真正交付的那件事 —— 而在此之前，那件事
  // 一条客户端断言都没有：唯一相关的那条断言的是它的对立面。
  //
  // # 为什么不在这里钉「只够得着 /workspace」
  //
  // 试过，钉不了：`ToolCall.path` 带的是**模型传进去的那个参数原样**
  //（实测拿到 `write_file@hello.txt`、`list_dir@.`），不是沙箱解析之后的绝对
  // 路径。围栏在服务端的 `cortex_agent::Sandbox` 里，客户端看不见它的输入，
  // 也看不见它的判断 —— 那条红线由那一侧的用例守，这里守的是「云端这条路
  // 真的接上了容器」。
  test('云端会话按需拉起容器，文件工具真的能用', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    final sessionId = 'live-sbx-${DateTime.now().millisecondsSinceEpoch}';
    var calls = <ToolCall>[];
    await for (final event in api.chat(
      sessionId: sessionId,
      message: '在工作区里建一个 hello.txt（内容随便），然后列一下工作区根目录。',
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

    // 只看**成功**的那些。模型完全可以凭空调一个不存在的工具（实测见过
    // `file_read`），daemon 照样会把这次尝试作为 tool 事件发出来 —— 而那条
    // 事件是失败的，界面也按失败画。把「出现过文件工具名」当成通过，
    // 测的就成了模型今天想怎么命名，而不是服务端到底给了什么工具。
    final succeeded = calls.where((c) => c.touchesFiles && !c.failed).toList();
    expect(
      succeeded,
      isNotEmpty,
      reason:
          '沙箱透明化之后，云端会话本来就该有能用的文件工具 —— 容器由第一次'
          '需要它的那一刻拉起。这条红了先看容器起没起来（docker ps）：'
          '在此之前的症状是「agent 说它没有文件工具」，而日志里一句异常都没有。'
          '这一轮实际拿到：${calls.map((c) => "${c.name}/${c.result}").toList()}',
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

  // ─────────────── 工具确认已经不在 cortexd 这一侧 ───────────────

  test('cortexd 不再有 /confirmations —— 确认属于 agent，而 agent 在别处', () async {
    if (!up) return markTestSkipped('cortexd not running');
    final api = _api();
    addTearDown(api.dispose);

    // 这里原本有三条用例：mock 后端用 `#confirm` 口令触发一次确认、
    // 从待办列表里捞回来、同一个 token 投两次。它们打的都是 cortexd 自己
    // 那个进程内 agent，而那个 agent 删掉了（见 cortexd `routes::chat`）。
    //
    // 换成钉住新契约的一条：**这个端点没了**。留着旧用例更糟 ——
    // `answerConfirmation` 对 404 返回 false，于是那条断言在路由被删之后
    // 照样是绿的，只是理由完全变了，而没人会发现。
    await expectLater(
      api.pendingConfirmations(),
      throwsA(
        isA<CortexApiException>().having(
          (e) => e.isMissing,
          'isMissing',
          isTrue,
        ),
      ),
      reason:
          '确认回路在 agent 那一侧：桌面端问本机 cortex-local，'
          '云端那一轮跑在容器里而容器里的 agent 压根不问',
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
