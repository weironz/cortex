import 'dart:async';
import 'package:cortex_app/models/import_plan.dart';
import 'package:cortex_app/import/import_source.dart';
import 'package:cortex_app/api/api_exception.dart';
import 'dart:typed_data';

import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/blob.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/models/episode.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/models/memory_search_result.dart';
import 'package:cortex_app/models/pending_confirmation.dart';
import 'package:cortex_app/models/session_detail.dart';
import 'package:cortex_app/models/sync_event.dart';
import 'package:cortex_app/models/sync_record.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/memory_controller.dart';
import 'package:cortex_app/state/sync_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// A `CortexApi` whose WebSocket is driven by the test.
///
/// The sync surface is driven by the test and the two refresh targets
/// (`/sessions`, `/memory/search`) are counted. Everything else throws, so a
/// controller that started a completion on a bump would fail loudly instead of
/// silently burning tokens.
class _FakeApi implements CortexApi {
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
  int searchCount = 0;

  @override
  Future<List<ChatSession>> sessions({bool includeArchived = false}) async {
    sessionsCount++;
    return const [];
  }

  @override
  Future<MemorySearchResult> searchMemory(
    String query, {
    int limit = 20,
    DateTime? asOf,
  }) async {
    searchCount++;
    return MemorySearchResult.empty;
  }

  @override
  String get label => 'fake';

  @override
  void dispose() {}

  @override
  Future<HealthStatus> health() =>
      throw UnimplementedError('同步链路不应触碰 /health');

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
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

  // Everything below is outside the sync surface. They throw rather than
  // returning a placeholder for the reason stated on the class: a controller
  // that reached for them on a bump should fail the test, not pass quietly.

  @override
  Future<SessionDetail> sessionDetail(String id, {int? limit, String? before}) =>
      throw UnimplementedError('同步链路不应拉会话详情');

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
  Future<Uint8List> blobBytes(String hash) => throw UnimplementedError();

  @override
  Future<AuthTicket> issueTicket() => throw UnimplementedError();

  @override
  Future<bool> answerConfirmation({
    required String token,
    required bool allow,
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

ProviderContainer _boot(_FakeApi api, {bool mock = false}) {
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(mock ? _MockConfig.new : _LiveConfig.new),
      cortexApiProvider.overrideWithValue(api),
    ],
  );
  // Keeps the controller alive for the duration of the test.
  container.listen(syncControllerProvider, (_, _) {}, fireImmediately: true);
  return container;
}

void main() {
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

    expect(
      api.sinceCalls,
      [10, 22],
      reason: '第二次仍从自己的 22 拉；用 25 会永久跳过 22..25 这一段',
    );
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
    expect(
      state.resyncs,
      2,
      reason: 'resync 混进 bump 就看不出服务端漏推过 —— 那正是要盯的运维信号',
    );
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

    expect(
      api.sinceCalls,
      [40],
      reason: '重连后补拉必须从断线前的 40 开始，而不是 hello 报的 99',
    );
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

    // The memory pane has run a query, so it has something to refresh.
    await container.read(memoryControllerProvider.notifier).search();
    final searchesBefore = api.searchCount;

    await _settle();
    api.emit(const SyncHello(cursor: 5, version: '0.0.1'));
    api.emit(const SyncBump(6));

    await _until(
      () => api.sessionsCount > 0 && api.searchCount > searchesBefore,
      reason: 'episodes 应刷新会话列表，facts 应刷新记忆面板',
    );
  });

  test('记忆面板没检索过就不替它发起检索', () async {
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
    expect(
      api.searchCount,
      0,
      reason: '面板还没被打开过就自动填充，用户会看到自己没要过的结果',
    );
  });

  test('mock 数据源不建立连接', () async {
    final api = _FakeApi();
    final container = _boot(api, mock: true);
    addTearDown(container.dispose);

    await _settle();
    expect(container.read(syncControllerProvider).status, SyncLinkStatus.disabled);
    expect(api.connectCount, 0, reason: 'mock 没有 daemon，连接尝试只会产生噪音');
  });
}
