import 'dart:async';
import 'dart:typed_data';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/blob.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/models/episode.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/models/memory_search_result.dart';
import 'package:cortex_app/models/session_detail.dart';
import 'package:cortex_app/models/sync_event.dart';
import 'package:cortex_app/models/sync_record.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/chat_state.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// A daemon stand-in whose `GET /sessions/{id}` the test controls.
///
/// Not the mock source: these cases are about the *shape* of the replay —
/// hundreds of episodes, a truncated response, a failing fetch — and the mock's
/// fixtures deliberately look like a real, small conversation.
class _ReplayApi implements CortexApi {
  _ReplayApi({required this.episodeCount, this.messageCount, this.fail = false});

  final int episodeCount;

  /// What the session claims to hold. Larger than [episodeCount] simulates the
  /// server's uncursored `LIMIT 500`.
  final int? messageCount;
  final bool fail;

  /// Ids passed to `sessionDetail`, in order.
  final List<String> detailCalls = [];

  ChatSession get _session => ChatSession(
    id: 's1',
    title: '很长的会话',
    messageCount: messageCount ?? episodeCount,
    updatedAt: DateTime(2026, 1, 1),
  );

  @override
  Future<List<ChatSession>> sessions({bool includeArchived = false}) async => [
    _session,
  ];

  @override
  Future<SessionDetail> sessionDetail(String id) async {
    detailCalls.add(id);
    if (fail) {
      throw const CortexApiException('数据库炸了', statusCode: 500);
    }
    return SessionDetail(
      session: _session,
      episodes: [
        for (var i = 0; i < episodeCount; i++)
          Episode(
            id: 'epi_$i',
            sessionId: 's1',
            role: i.isEven ? 'user' : 'assistant',
            text: '第 $i 条',
            occurredAt: DateTime(2026, 1, 1).add(Duration(minutes: i)),
            attachments: i == 0
                ? const [Attachment(hash: 'abc123def456', kind: 'image')]
                : const [],
          ),
      ],
    );
  }

  @override
  String get label => 'replay';

  @override
  void dispose() {}

  @override
  Future<HealthStatus> health() async =>
      const HealthStatus(status: 'ok', version: 't', database: 'ok');

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
  }) async* {
    yield const ChatDeltaEvent('好');
    yield const ChatDoneEvent('epi_new');
  }

  @override
  Future<MemorySearchResult> searchMemory(
    String query, {
    int limit = 20,
    DateTime? asOf,
  }) async => MemorySearchResult.empty;

  @override
  Future<Episode> episode(String id) => throw UnimplementedError();

  @override
  Future<ChatSession> updateSession(
    String id, {
    String? title,
    bool? archived,
    String? workspace,
    bool clearWorkspace = false,
  }) => throw UnimplementedError();

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
  Future<Uint8List> blobBytes(String hash) async => Uint8List(0);

  @override
  Stream<SyncEvent> watchSync() => const Stream.empty();

  @override
  Future<SyncPage> sync({required int since, int limit = 500}) async =>
      SyncPage(cursor: since);
}

class _Config extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:8080');
}

ProviderContainer _boot(_ReplayApi api) {
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_Config.new),
      cortexApiProvider.overrideWithValue(api),
    ],
  );
  container.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
  return container;
}

Future<void> _until(
  bool Function() condition, {
  String reason = '',
  Duration timeout = const Duration(seconds: 10),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

/// Nullable on purpose: several cases wait for the transcript to *appear*, and
/// a `!` here would throw inside the poll loop instead of looping.
Transcript? _peek(ProviderContainer c) =>
    c.read(chatControllerProvider).transcripts['s1'];

Transcript _transcript(ProviderContainer c) => _peek(c)!;

void main() {
  group('历史会话回放', () {
    test('打开会话时拉取 GET /sessions/{id}，附件一并回来', () async {
      final container = _boot(_ReplayApi(episodeCount: 4));
      addTearDown(container.dispose);

      await _until(
        () => _peek(container)?.loadedFromServer ?? false,
        reason: '首次加载',
      );

      final t = _transcript(container);
      expect(t.messages, hasLength(4));
      expect(t.messages.first.role, MessageRole.user);
      expect(t.messages[1].role, MessageRole.assistant);
      expect(
        t.messages.first.attachments.single.hash,
        'abc123def456',
        reason: 'episode 上挂的附件必须跟着回放出来',
      );
      expect(
        t.messages.first.episodeId,
        'epi_0',
        reason: '回放出来的消息要能追溯到 episode',
      );
    });

    test('只渲染最新一屏，往上翻才展开更早的', () async {
      final container = _boot(_ReplayApi(episodeCount: 130));
      addTearDown(container.dispose);
      await _until(
        () => _peek(container)?.loadedFromServer ?? false,
        reason: '首次加载',
      );

      var t = _transcript(container);
      expect(t.messages, hasLength(130));
      expect(
        t.visible,
        hasLength(kTranscriptWindow),
        reason: '一次性渲染 130 条 markdown 气泡是没必要的开销',
      );
      expect(
        t.visible.last.text,
        '第 129 条',
        reason: '窗口从最新的一端取 —— 打开会话应看到结尾而不是开头',
      );
      expect(t.hasEarlier, isTrue);

      container.read(chatControllerProvider.notifier).revealEarlier('s1');
      t = _transcript(container);
      expect(t.visible, hasLength(kTranscriptWindow * 2));
      expect(t.visible.last.text, '第 129 条', reason: '展开是往上长，不是换一页');
    });

    test('可见列表的引用在没有变化时保持不变', () async {
      final container = _boot(_ReplayApi(episodeCount: 130));
      addTearDown(container.dispose);
      await _until(
        () => _peek(container)?.loadedFromServer ?? false,
        reason: '首次加载',
      );

      // `select` 用 `==` 比较。如果 visible 是每次现算的 sublist，这里就会
      // 拿到两个不同的对象，于是每个 SSE delta 都会重建整个 ListView ——
      // 正是流式渲染千方百计要避免的那笔开销。
      final a = container.read(chatControllerProvider).activeVisibleMessages;
      final b = container.read(chatControllerProvider).activeVisibleMessages;
      expect(identical(a, b), isTrue, reason: '可见列表必须是同一个对象');
    });

    test('服务端截断时明确告知，而不是默默少几条', () async {
      final container = _boot(
        // The server capped at 500 but the session holds 900.
        _ReplayApi(episodeCount: 500, messageCount: 900),
      );
      addTearDown(container.dispose);
      await _until(
        () => _peek(container)?.loadedFromServer ?? false,
        reason: '首次加载',
      );

      expect(
        _transcript(container).serverTruncated,
        isTrue,
        reason: 'LIMIT 500 是升序截断，缺的是最新几轮 —— 不说就等于展示了一段假历史',
      );
    });

    test('条数对得上时不报截断', () async {
      final container = _boot(_ReplayApi(episodeCount: 12));
      addTearDown(container.dispose);
      await _until(
        () => _peek(container)?.loadedFromServer ?? false,
        reason: '首次加载',
      );
      expect(_transcript(container).serverTruncated, isFalse);
    });

    test('拉取失败可重试，且不会被当成空会话', () async {
      final failing = _ReplayApi(episodeCount: 3, fail: true);
      final container = _boot(failing);
      addTearDown(container.dispose);

      await _until(
        () => _peek(container)?.error != null,
        reason: '错误落到 transcript 上',
      );
      final t = _transcript(container);
      expect(t.error, contains('数据库炸了'));
      expect(
        t.loadedFromServer,
        isFalse,
        reason: '失败不等于「已加载且是空的」，否则重试入口就没了',
      );
    });

    test('已经打开过的会话不会被重复拉取', () async {
      final api = _ReplayApi(episodeCount: 3);
      final container = _boot(api);
      addTearDown(container.dispose);
      await _until(
        () => _peek(container)?.loadedFromServer ?? false,
        reason: '首次加载',
      );

      final controller = container.read(chatControllerProvider.notifier);
      controller.selectSession('other');
      controller.selectSession('s1');
      await Future<void>.delayed(const Duration(milliseconds: 50));

      expect(
        api.detailCalls.where((id) => id == 's1'),
        hasLength(1),
        reason:
            '重复拉取会把本地已发出的用户消息复制一份 —— 客户端不知道自己那条 '
            'prompt 的 episode id，去重无从下手',
      );
    });

    test('本地新建的草稿不会去拉一个服务端没有的会话', () async {
      final api = _ReplayApi(episodeCount: 3);
      final container = _boot(api);
      addTearDown(container.dispose);
      await _until(
        () => _peek(container)?.loadedFromServer ?? false,
        reason: '首次加载',
      );

      final before = api.detailCalls.length;
      container.read(chatControllerProvider.notifier).createSession();
      await Future<void>.delayed(const Duration(milliseconds: 50));
      expect(
        api.detailCalls,
        hasLength(before),
        reason: '草稿在服务端还不存在，拉它只会显示一个假错误',
      );
    });

    test('新发的消息落在窗口内，不会被挤到窗口外面去', () async {
      final container = _boot(_ReplayApi(episodeCount: 130));
      addTearDown(container.dispose);
      await _until(
        () => _peek(container)?.loadedFromServer ?? false,
        reason: '首次加载',
      );

      await container.read(chatControllerProvider.notifier).send('新问题');
      await _until(
        () => container.read(chatControllerProvider).streaming == null,
        reason: '生成结束',
      );

      final visible = _transcript(container).visible;
      expect(
        visible.any((m) => m.text == '新问题'),
        isTrue,
        reason: '刚发出去的消息必须看得见',
      );
      expect(visible.last.role, MessageRole.assistant);
    });
  });
}
