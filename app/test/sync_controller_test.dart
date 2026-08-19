import 'package:cortex_app/core/permission_mode.dart';
import 'dart:async';
import 'package:cortex_app/models/auth_tokens.dart';
import 'package:cortex_app/models/import_plan.dart';
import 'package:cortex_app/import/import_source.dart';
import 'package:cortex_app/api/api_exception.dart';
import 'dart:typed_data';

import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/models/workspace.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/blob.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/models/episode.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/models/pending_confirmation.dart';
import 'package:cortex_app/models/project.dart';
import 'package:cortex_app/models/session_detail.dart';
import 'package:cortex_app/models/sync_event.dart';
import 'package:cortex_app/models/sync_record.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:cortex_app/state/sync_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// A `CortexApi` whose WebSocket is driven by the test.
///
/// The sync surface is driven by the test and the two refresh targets
/// (`/sessions`, `/memory/search`) are counted. Everything else throws, so a
/// controller that started a completion on a bump would fail loudly instead of
/// silently burning tokens.
class _FakeApi
    with
        LlmKeyUnsupported,
        AccountUnsupported,
        LocalWorkspaceUnsupported,
        LocalMcpUnsupported,
        RunAttachUnsupported,
        SessionSearchUnsupported,
        SandboxHealthUnsupported
    implements CortexApi {
  int connectCount = 0;

  /// Every `since` the controller has asked for, in order. This list is the
  /// whole point of the suite.
  final List<int> sinceCalls = [];

  StreamController<SyncEvent>? _link;

  /// Answers `GET /sync`. Defaults to "you are already caught up".
  SyncPage Function(int since) respond = (since) => SyncPage(cursor: since);

  /// 没有本地 agent 的替身：这条路由不存在，报「不支持」正是让调用方
  /// 去走 `PATCH /sessions/{id}` 回落分支的那个信号。
  /// 测试替身不做导入 —— 报「不支持」，让调用方走它的降级分支。
  @override
  Future<AuthTokens> login(String username, String password) async {
    throw const CortexApiException('这个数据源不支持账号登录', statusCode: 501);
  }

  @override
  Future<AuthTokens> refreshSession(String refreshToken) async {
    throw const CortexApiException('这个数据源不支持账号登录', statusCode: 501);
  }

  @override
  Future<void> logout(String refreshToken) async {}

  @override
  Future<ImportTarget> prepareImport(ImportSource source) async {
    throw const CortexApiException('测试替身不支持导入', statusCode: 501);
  }

  @override
  Future<ImportEstimate> importPreview(
    ImportTarget target, {
    int? maxConversations,
  }) async {
    throw const CortexApiException('测试替身不支持导入', statusCode: 501);
  }

  @override
  Stream<ImportEvent> runImport(ImportTarget target, {int? maxConversations}) =>
      Stream.error(const CortexApiException('测试替身不支持导入', statusCode: 501));

  @override
  Future<String?> bindLocalWorkspace(String id, String? path) async {
    throw const CortexApiException('测试替身没有本地工作区端点', statusCode: 404);
  }

  @override
  Stream<SyncEvent> watchSync() {
    connectCount++;
    final controller = StreamController<SyncEvent>();
    _link = controller;
    return controller.stream;
  }

  void emit(SyncEvent event) => _link!.add(event);

  /// Server-side close, i.e. `cortexd` restarting.
  Future<void> drop() => _link!.close();

  @override
  Future<SyncPage> sync({required int since, int limit = 500}) async {
    sinceCalls.add(since);
    return respond(since);
  }

  int sessionsCount = 0;

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async {
    sessionsCount++;
    return const [];
  }

  @override
  Future<List<Project>> projects() async => const [];

  @override
  Future<Project> createProject(String name) => throw UnimplementedError();

  @override
  Future<Project> renameProject(String id, String name) =>
      throw UnimplementedError();

  @override
  Future<void> deleteProject(String id) => throw UnimplementedError();

  @override
  Future<ChatSession> moveSessionToProject(
    String sessionId,
    String? projectId,
  ) => throw UnimplementedError();

  @override
  Future<ChatSession> setContainerWorkspace(String sessionId, String? name) =>
      throw UnimplementedError();

  @override
  Future<void> stopRun(String sessionId) => throw UnimplementedError();

  @override
  String get label => 'fake';

  @override
  void dispose() {}

  @override
  Future<HealthStatus> health() => throw UnimplementedError('同步链路不应触碰 /health');

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    bool sandbox = false,
  }) => throw UnimplementedError('同步链路不应发起对话');

  @override
  Future<Episode> episode(String id) => throw UnimplementedError();

  /// Every `GET /confirmations` the controller made. Not a throwing stub: the
  /// recovery poll on `hello` is part of the sync surface now, and the count is
  /// what one of the cases below asserts on.
  int confirmPolls = 0;

  @override
  Future<List<PendingConfirmation>> pendingConfirmations({
    String? sessionId,
  }) async {
    confirmPolls++;
    return const [];
  }

  /// 拉过详情的会话 id，按顺序。
  ///
  /// 这里**曾经是一个 throw**，注释写着「同步链路不应拉会话详情」——
  /// 2026-08-15 真机把那条假设证伪了：只刷列表不刷正在看的那一段，
  /// 表现就是「侧栏变成刚刚了，对话停在几分钟前」。
  final List<String> detailCalls = [];

  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async {
    detailCalls.add(id);
    return SessionDetail(
      session: ChatSession(
        id: id,
        title: '',
        messageCount: 0,
        updatedAt: DateTime(2026),
      ),
      episodes: const [],
      hasMore: false,
    );
  }

  // Everything below is outside the sync surface. They throw rather than
  // returning a placeholder for the reason stated on the class: a controller
  // that reached for them on a bump should fail the test, not pass quietly.

  @override
  Future<ChatSession> updateSession(
    String id, {
    String? title,
    bool? archived,
    String? workspace,
    bool clearWorkspace = false,
  }) => throw UnimplementedError('同步链路不应改会话');

  @override
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) => throw UnimplementedError();

  @override
  Future<BlobPresign> presignBlob(String hash) => throw UnimplementedError();

  @override
  Future<void> putPresigned({
    required String url,
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) => throw UnimplementedError();

  @override
  Future<BlobRef> commitBlob({
    required String hash,
    required int sizeBytes,
    String? mime,
  }) => throw UnimplementedError();

  @override
  Future<Uint8List> sandboxWorkspaceTar({String? sessionId}) async =>
      throw UnimplementedError('这个替身不测云沙箱下载');

  @override
  Future<List<FileNode>> sandboxListFiles(
    String path, {
    String? sessionId,
  }) async => throw UnimplementedError('这个替身不测云沙箱文件树');

  @override
  Future<Uint8List> sandboxReadFile(String path, {String? sessionId}) async =>
      throw UnimplementedError('这个替身不测云沙箱文件树');

  @override
  Future<SandboxWriteReceipt> sandboxWriteFile({
    required String path,
    required Uint8List bytes,
    UploadProgress? onProgress,
    String? sessionId,
  }) async => throw UnimplementedError('这个替身不测云沙箱文件树');

  @override
  Future<Uint8List> blobBytes(String hash) => throw UnimplementedError();

  @override
  Future<AuthTicket> issueTicket() => throw UnimplementedError();

  @override
  Future<bool> answerConfirmation({
    required String token,
    required bool allow,
    String? sessionId,
  }) => throw UnimplementedError('同步链路不应投递回执');
}

class _LiveConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:8080');
}

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

Future<void> _settle([int turns = 6]) async {
  for (var i = 0; i < turns; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

Future<void> _until(bool Function() condition, {String reason = ''}) async {
  final deadline = DateTime.now().add(const Duration(seconds: 5));
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 20));
  }
}

/// 已登录的替身。sync 现在有了「没登录就不连」的门（登录页阶段的 401
/// 红字正是没这道门造成的），所以这套测试要先把门打开，才能测门后面的东西。
class _ReadyAuth extends AuthController {
  @override
  AuthState build() => const AuthState(phase: AuthPhase.ready, token: 't');
}

/// 还没登录的替身。
class _GateClosedAuth extends AuthController {
  @override
  AuthState build() => const AuthState(phase: AuthPhase.needsToken);
}

ProviderContainer _boot(_FakeApi api, {bool mock = false}) {
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(mock ? _MockConfig.new : _LiveConfig.new),
      cortexApiProvider.overrideWithValue(api),
      authControllerProvider.overrideWith(_ReadyAuth.new),
    ],
  );
  // Keeps the controller alive for the duration of the test.
  container.listen(syncControllerProvider, (_, _) {}, fireImmediately: true);
  return container;
}

void main() {
  test('没登录就一个请求都不发 —— 登录页上的红字就是这么来的', () async {
    final api = _FakeApi();
    final container = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_LiveConfig.new),
        cortexApiProvider.overrideWithValue(api),
        authControllerProvider.overrideWith(_GateClosedAuth.new),
      ],
    );
    addTearDown(container.dispose);
    container.listen(syncControllerProvider, (_, _) {}, fireImmediately: true);
    await _settle();

    expect(
      api.connectCount,
      0,
      reason:
          '未登录阶段连 WS、要 ticket，得到的只能是 401 —— '
          '而那个 401 会把「凭据已失效」压在登录表单上',
    );
    expect(
      container.read(syncControllerProvider).status,
      SyncLinkStatus.disabled,
    );
  });

  test('bump 只是信号：拉取一律用客户端自己的游标', () async {
    final api = _FakeApi()
      // The server says it reached 25, but this page only carries us to 22 —
      // the gap 22..25 is exactly what a client that trusted the event cursor
      // would lose forever.
      ..respond = (since) => SyncPage(
        cursor: 22,
        records: const [SyncRecord(seq: 22, table: 'facts', id: 'f1')],
      );
    final container = _boot(api);
    addTearDown(container.dispose);

    await _settle();
    expect(api.connectCount, 1);

    api.emit(const SyncHello(cursor: 10, version: '0.0.1'));
    await _settle();

    var state = container.read(syncControllerProvider);
    expect(state.status, SyncLinkStatus.live);
    expect(state.cursor, 10, reason: '首次连接没有本地存储，以 hello 的游标为基线');
    expect(api.sinceCalls, isEmpty, reason: '与服务端持平时不该发起补拉');

    api.emit(const SyncBump(25));
    await _settle();

    expect(api.sinceCalls, [10], reason: 'since 必须是自己的 10，不是事件里的 25');
    state = container.read(syncControllerProvider);
    expect(state.cursor, 22, reason: '游标只能从 /sync 响应推进');
    expect(state.serverCursor, 25);
    expect(state.isBehind, isTrue);
    expect(state.bumps, 1);

    api.emit(const SyncBump(30));
    await _settle();

    expect(api.sinceCalls, [
      10,
      22,
    ], reason: '第二次仍从自己的 22 拉；用 25 会永久跳过 22..25 这一段');
  });

  test('resync 与 bump 分开计数', () async {
    final api = _FakeApi();
    final container = _boot(api);
    addTearDown(container.dispose);

    await _settle();
    api.emit(const SyncHello(cursor: 0, version: '0.0.1'));
    api.emit(const SyncBump(1));
    api.emit(const SyncResync(2));
    api.emit(const SyncResync(3));
    await _settle();

    final state = container.read(syncControllerProvider);
    expect(state.bumps, 1);
    expect(state.resyncs, 2, reason: 'resync 混进 bump 就看不出服务端漏推过 —— 那正是要盯的运维信号');
  });

  test('断线后指数退避重连，且不吞掉自己的游标', () async {
    final api = _FakeApi()..respond = (since) => SyncPage(cursor: since);
    final container = _boot(api);
    addTearDown(container.dispose);

    await _settle();
    api.emit(const SyncHello(cursor: 40, version: '0.0.1'));
    await _settle();

    await api.drop();
    await _settle();

    var state = container.read(syncControllerProvider);
    expect(state.status, SyncLinkStatus.reconnecting);
    expect(state.attempt, 1);
    expect(state.nextRetryAt, isNotNull, reason: '必须已经排好下一次重试');

    await _until(() => api.connectCount == 2, reason: '退避到期后应自动重连');

    // A daemon that restarted may be far ahead; adopting its cursor would skip
    // everything committed while we were offline.
    api.emit(const SyncHello(cursor: 99, version: '0.0.1'));
    await _settle();

    expect(api.sinceCalls, [40], reason: '重连后补拉必须从断线前的 40 开始，而不是 hello 报的 99');
    expect(container.read(syncControllerProvider).attempt, 0);
    expect(container.read(syncControllerProvider).status, SyncLinkStatus.live);
  });

  test('每次 hello 都去捞一遍待确认的工具调用', () async {
    // SSE 流是一次性的、没有 Last-Event-ID 重放：断线时在途的那条确认请求
    // 再也不会重发。而确认事件刻意**不**走 /ws 广播（那条通道的契约是
    // 「只推信号不推数据」），所以 hello 之后拉一次 GET /confirmations
    // 是「有没有什么还等着我批」唯一的答案。
    // 没有它，那一轮会一直挂到超时，而界面上没有任何东西解释为什么卡住了。
    final api = _FakeApi();
    final container = _boot(api);
    addTearDown(container.dispose);

    await _settle();
    api.emit(const SyncHello(cursor: 10, version: '0.0.1'));
    await _settle();
    // 冷启动本身也会拉一次（对服务端来说，「刚打开」与「刚重连」没有区别），
    // 所以这里钉的是**增量**而不是绝对次数
    final afterFirstHello = api.confirmPolls;
    expect(afterFirstHello, greaterThanOrEqualTo(1));

    await api.drop();
    await _settle();
    await _until(() => api.connectCount == 2, reason: '退避到期后应自动重连');
    api.emit(const SyncHello(cursor: 10, version: '0.0.1'));
    await _settle();

    expect(
      api.confirmPolls,
      afterFirstHello + 1,
      reason: '重连本身就是那个「刚才断的时候有没有漏掉什么」的时刻',
    );
  });

  test('服务端谎报 has_more 也不会把客户端转死', () async {
    // A daemon bug (or a stale read replica) could return has_more with an
    // unchanged cursor. The loop must terminate on the non-advance rather than
    // trusting the flag.
    final api = _FakeApi()
      ..respond = (since) => SyncPage(cursor: since, hasMore: true);
    final container = _boot(api);
    addTearDown(container.dispose);

    await _settle();
    api.emit(const SyncHello(cursor: 5, version: '0.0.1'));
    api.emit(const SyncBump(6));
    await _settle();

    expect(api.sinceCalls, hasLength(1));
  });

  test('变更的表决定刷新哪个面板', () async {
    final api = _FakeApi()
      ..respond = (since) => SyncPage(
        cursor: since + 1,
        records: [
          SyncRecord(seq: since + 1, table: 'episodes', id: 'e1'),
          SyncRecord(seq: since + 1, table: 'facts', id: 'f1'),
        ],
      );
    final container = _boot(api);
    addTearDown(container.dispose);

    await _settle();
    api.emit(const SyncHello(cursor: 5, version: '0.0.1'));
    api.emit(const SyncBump(6));

    await _until(() => api.sessionsCount > 0, reason: 'episodes 应刷新会话列表');
  });

  /// **列表刷新了不等于对话刷新了。**
  ///
  /// 真机症状（2026-08-15）：在浏览器里聊完切到桌面端，侧栏那条已经变成
  /// 「刚刚」并排到最前，对话却停在几分钟前的最后一句 —— 用户的结论是
  /// 「不是实时同步的吧」，而 WS bump → /sync 那条链路全程是通的。
  ///
  /// 漏的是最后一步：`loadSessions` 里那个 `_ensureTranscript` 的语义是
  /// 「没加载过才加载」，打开着的会话必然已经在 map 里，于是每次都提前
  /// return。**看起来处理了**，这正是它活了这么久的原因。
  test('打开着的会话也要重载，不只是列表', () async {
    final api = _FakeApi()
      ..respond = (since) => SyncPage(
        cursor: since + 1,
        records: [SyncRecord(seq: since + 1, table: 'episodes', id: 'e1')],
      );
    final container = _boot(api);
    addTearDown(container.dispose);

    // 先把 ChatController 建出来，让它 build 里那个 `Future.microtask(_reload)`
    // 跑完 —— `_reload` 会把 state 整个重置，在它之前选中会被抹掉。
    // 真实应用里这一步在开机时就完成了，这里只是把顺序摆对。
    container.read(chatControllerProvider);
    await _settle();
    container.read(chatControllerProvider.notifier).selectSession('S-open');
    await _settle();
    expect(
      container.read(chatControllerProvider).activeSessionId,
      'S-open',
      reason: '前提：这个会话此刻是打开着的',
    );
    expect(api.detailCalls, contains('S-open'), reason: '前提：选中时已拉过一次详情');
    api.detailCalls.clear();

    api.emit(const SyncHello(cursor: 5, version: '0.0.1'));
    api.emit(const SyncBump(6));

    await _until(
      () => api.detailCalls.contains('S-open'),
      reason: '正在看的那一段必须跟着重载，否则新消息要等到切走再切回来才出现',
    );
  });

  /// **服务端发的每一张「对话类」表都得触发刷新。**
  ///
  /// `session_events` / `project_events` 服务端一直在发（`SyncPayload` 与
  /// `sync_payload::to_json` 两处都齐），而客户端的 `SyncTables.conversation`
  /// 里没有它们 —— 帧到了、游标推进了，界面上什么都不动。
  ///
  /// 症状是「同步好像有点慢」：另一台设备上改个会话标题、建或删一个项目，
  /// 这台机器要等下一次整体重拉才看得见，而链路全程是通的、不报任何错。
  ///
  /// 逐个表跑而不是一次塞进去：一次全给的话，只要有一张命中测试就绿，
  /// 而漏掉的那张正是会被漏掉的那张。
  test('会话与项目的变更都会触发重拉', () async {
    for (final table in const [
      'episodes',
      'episode_tool_calls',
      'session_events',
      'project_events',
    ]) {
      final api = _FakeApi()
        ..respond = (since) => SyncPage(
          cursor: since + 1,
          records: [SyncRecord(seq: since + 1, table: table, id: 'x')],
        );
      final container = _boot(api);
      addTearDown(container.dispose);

      await _settle();
      api.emit(const SyncHello(cursor: 1, version: '0.0.1'));
      api.emit(const SyncBump(2));
      await Future<void>.delayed(const Duration(milliseconds: 600));

      expect(
        api.sessionsCount,
        greaterThan(0),
        reason:
            '$table 变了却没重拉会话列表 —— 服务端发了这张表，'
            '而 SyncTables.conversation 里没有它，于是帧到了、游标走了、界面不动',
      );
    }
  });

  /// `facts` 那几张表**照旧下发，但这一侧不再因此刷新任何东西** ——
  /// 记忆界面去了 Cormex。
  ///
  /// 这条钉的是「不该有反应」而不是「有反应」：删界面时顺手把
  /// `SyncTables.memory` 也删掉，症状是服务端下发的表清单与客户端认得的
  /// 对不上，而那是一条到部署才炸的错。
  test('facts 变更不再触发这一侧的任何请求', () async {
    final api = _FakeApi()
      ..respond = (since) => SyncPage(
        cursor: since + 1,
        records: [SyncRecord(seq: since + 1, table: 'facts', id: 'f1')],
      );
    final container = _boot(api);
    addTearDown(container.dispose);

    await _settle();
    api.emit(const SyncHello(cursor: 1, version: '0.0.1'));
    api.emit(const SyncBump(2));

    // Long enough to clear the refresh debounce.
    await Future<void>.delayed(const Duration(milliseconds: 600));
    expect(api.sessionsCount, 0, reason: 'facts 与会话列表无关，不该顺带把它也拉一遍');
  });

  test('mock 数据源不建立连接', () async {
    final api = _FakeApi();
    final container = _boot(api, mock: true);
    addTearDown(container.dispose);

    await _settle();
    expect(
      container.read(syncControllerProvider).status,
      SyncLinkStatus.disabled,
    );
    expect(api.connectCount, 0, reason: 'mock 没有 daemon，连接尝试只会产生噪音');
  });
}
