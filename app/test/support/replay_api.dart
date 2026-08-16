/// Shared test double: a daemon whose `GET /sessions/{id}` really pages.
///
/// Lives here rather than in one test file because both the controller cases
/// (`history_replay_test.dart`) and the widget wiring case (`widget_test.dart`)
/// need the same paging behaviour, and two copies would drift.
library;

import 'package:cortex_app/core/permission_mode.dart';
import 'dart:typed_data';
import 'package:cortex_app/models/auth_tokens.dart';
import 'package:cortex_app/models/import_plan.dart';
import 'package:cortex_app/import/import_source.dart';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/models/workspace.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/blob.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/models/episode.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/models/pending_confirmation.dart';
import 'package:cortex_app/models/project.dart';
import 'package:cortex_app/models/session_detail.dart';
import 'package:cortex_app/models/sync_event.dart';
import 'package:cortex_app/models/sync_record.dart';

/// A daemon stand-in that pages `GET /sessions/{id}` the way cortexd does.
///
/// Not the mock source: these cases are about the *shape* of the replay —
/// hundreds of episodes, a page walk, a failing fetch — and the mock's fixtures
/// deliberately look like a real, small conversation.
///
/// The cursor here is an episode id rather than cortexd's `<time>|<ulid>`. The
/// client treats it as opaque either way; using a different shape is a cheap
/// check that it really does.
class ReplayApi
    with
        LlmKeyUnsupported,
        AccountUnsupported,
        LocalWorkspaceUnsupported,
        LocalMcpUnsupported,
        RunAttachUnsupported,
        SandboxHealthUnsupported
    implements CortexApi {
  ReplayApi({
    required this.episodeCount,
    this.fail = false,
    this.failEarlier = false,
    this.episodes,
  });

  final int episodeCount;
  final bool fail;

  /// The *first* page succeeds and every page after it fails. Models a daemon
  /// that goes away mid-scroll.
  final bool failEarlier;

  /// Overrides the generated transcript; used by the attribution cases.
  final List<Episode>? episodes;

  /// `(limit, before)` for every call, in order.
  final List<(String, int?, String?)> detailCalls = [];

  ChatSession get _session => ChatSession(
    id: 's1',
    title: '很长的会话',
    messageCount: episodeCount,
    updatedAt: DateTime(2026, 1, 1),
  );

  late final List<Episode> _all =
      episodes ??
      [
        for (var i = 0; i < episodeCount; i++)
          Episode(
            id: 'epi_$i',
            sessionId: 's1',
            role: i.isEven ? 'user' : 'assistant',
            text: '第 $i 条',
            occurredAt: DateTime(2026, 1, 1).add(Duration(minutes: i)),
            attachments: i == 0
                ? const [
                    Attachment(
                      hash: 'abc123def456',
                      kind: 'image',
                      filename: '设计稿.png',
                      mime: 'image/png',
                      sizeBytes: 40960,
                    ),
                  ]
                : const [],
          ),
      ];

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
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => [_session];

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
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async {
    detailCalls.add((id, limit, before));
    if (fail || (failEarlier && before != null)) {
      throw const CortexApiException('数据库炸了', statusCode: 500);
    }

    var upTo = _all.length;
    if (before != null) {
      upTo = _all.indexWhere((e) => e.id == before);
      if (upTo < 0) {
        throw const CortexApiException('游标不认识', statusCode: 400);
      }
    }
    final size = limit ?? 500;
    final start = upTo - size < 0 ? 0 : upTo - size;
    final page = _all.sublist(start, upTo);
    return SessionDetail(
      session: _session,
      episodes: page,
      hasMore: start > 0,
      nextCursor: start > 0 && page.isNotEmpty ? page.first.id : null,
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
    PermissionMode permissionMode = PermissionMode.ask,
    bool sandbox = false,
  }) async* {
    yield const ChatDeltaEvent('好');
    yield const ChatDoneEvent('epi_new');
  }

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
  Future<Uint8List> blobBytes(String hash) async => Uint8List(0);

  @override
  Future<AuthTicket> issueTicket() => throw UnimplementedError();

  /// Empty rather than throwing: `ConfirmController` polls this on start, and a
  /// replay fixture with nothing pending is the honest answer.
  @override
  Future<List<PendingConfirmation>> pendingConfirmations({
    String? sessionId,
  }) async => const [];

  @override
  Future<bool> answerConfirmation({
    required String token,
    required bool allow,
    String? sessionId,
  }) => throw UnimplementedError();

  @override
  Stream<SyncEvent> watchSync() => const Stream.empty();

  @override
  Future<SyncPage> sync({required int since, int limit = 500}) async =>
      SyncPage(cursor: since);
}
