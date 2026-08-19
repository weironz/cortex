import 'package:cortex_app/core/permission_mode.dart';
import 'dart:async';
import 'package:cortex_app/models/auth_tokens.dart';
import 'package:cortex_app/models/import_plan.dart';
import 'package:cortex_app/import/import_source.dart';
import 'dart:typed_data';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/blob_upload.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/models/workspace.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/hashing.dart';
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
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/attachment_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// Records which upload route was taken.
///
/// The whole point of these cases is the *routing decision* — relay versus
/// presign — so the fake reports the path rather than the payload.
class _BlobApi
    with
        LlmKeyUnsupported,
        AccountUnsupported,
        LocalWorkspaceUnsupported,
        LocalMcpUnsupported,
        RunAttachUnsupported,
        SessionSearchUnsupported,
        UsageUnsupported,
        SandboxHealthUnsupported
    implements CortexApi {
  _BlobApi({this.presignSupported = true, this.alreadyUploaded = false});

  final bool presignSupported;
  final bool alreadyUploaded;

  final List<String> route = [];
  List<Attachment> lastSentAttachments = const [];

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
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async {
    route.add('relay');
    if (bytes.length > kRelayUploadLimit) {
      throw const CortexApiException('请求体过大', statusCode: 413);
    }
    onProgress?.call(bytes.length ~/ 2, bytes.length);
    onProgress?.call(bytes.length, bytes.length);
    return BlobRef(
      hash: await sha256Hex(bytes),
      mime: mime ?? 'application/octet-stream',
      sizeBytes: bytes.length,
    );
  }

  @override
  Future<BlobPresign> presignBlob(String hash) async {
    route.add('presign');
    if (!presignSupported) {
      throw const CortexApiException(
        '对象存储后端 local_fs 签不出 presigned URL',
        statusCode: 501,
      );
    }
    return BlobPresign(
      url: 'https://bucket.example/$hash',
      method: 'PUT',
      expiresInSecs: 900,
      alreadyUploaded: alreadyUploaded,
    );
  }

  @override
  Future<void> putPresigned({
    required String url,
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async {
    route.add('put');
    onProgress?.call(bytes.length, bytes.length);
  }

  @override
  Future<BlobRef> commitBlob({
    required String hash,
    required int sizeBytes,
    String? mime,
  }) async {
    route.add('commit');
    return BlobRef(
      hash: hash,
      mime: mime ?? 'application/octet-stream',
      sizeBytes: sizeBytes,
    );
  }

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
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    bool sandbox = false,
  }) async* {
    lastSentAttachments = attachments;
    yield const ChatDoneEvent('epi_1');
  }

  @override
  String get label => 'blob-fake';

  @override
  void dispose() {}

  @override
  Future<HealthStatus> health() async =>
      const HealthStatus(status: 'ok', version: 't', database: 'ok');

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => const [];

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
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) => throw UnimplementedError();

  @override
  Future<ChatSession> updateSession(
    String id, {
    String? title,
    bool? archived,
    String? workspace,
    bool clearWorkspace = false,
  }) => throw UnimplementedError();

  @override
  Future<Episode> episode(String id) => throw UnimplementedError();

  @override
  Stream<SyncEvent> watchSync() => const Stream.empty();

  @override
  Future<SyncPage> sync({required int since, int limit = 500}) async =>
      SyncPage(cursor: since);

  @override
  Future<AuthTicket> issueTicket() => throw UnimplementedError();

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
}

class _Config extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:8080');
}

ProviderContainer _boot(CortexApi api) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_Config.new),
    cortexApiProvider.overrideWithValue(api),
  ],
);

/// Allocates without actually touching that much RAM where possible: the
/// routing decision only reads `bytes.length`.
Uint8List _bytes(int n) => Uint8List(n);

Future<void> _until(
  bool Function() condition, {
  String reason = '',
  Duration timeout = const Duration(seconds: 20),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

void main() {
  group('上传路由的门槛', () {
    test('门槛与 cortexd 的 DIRECT_UPLOAD_LIMIT 一致', () {
      expect(
        kRelayUploadLimit,
        32 * 1024 * 1024,
        reason:
            'POST /blobs 的 DefaultBodyLimit 就是这个数。客户端定得更高只会换来 '
            '413，定得更低会把小文件推上三次往返的直传路径',
      );
    });

    test('门槛之内走中转，一次往返', () async {
      final api = _BlobApi();
      final attachment = await uploadAttachment(
        api,
        bytes: _bytes(1024),
        filename: 'note.txt',
      );
      expect(api.route, ['relay']);
      expect(attachment.filename, 'note.txt');
      expect(attachment.kind, 'document');
    });

    test('正好在门槛上仍走中转', () async {
      final api = _BlobApi();
      await uploadAttachment(
        api,
        bytes: _bytes(kRelayUploadLimit),
        filename: 'big.bin',
      );
      expect(api.route, ['relay'], reason: '上限是闭区间，不然刚好卡住的文件两条路都走不通');
    });

    test('超过门槛走直传：presign → put → commit', () async {
      final api = _BlobApi();
      await uploadAttachment(
        api,
        bytes: _bytes(kRelayUploadLimit + 1),
        filename: 'movie.mp4',
      );
      expect(api.route, ['presign', 'put', 'commit']);
    });

    test('对象存储已有同样内容时跳过上传', () async {
      final api = _BlobApi(alreadyUploaded: true);
      var reported = 0;
      await uploadAttachment(
        api,
        bytes: _bytes(kRelayUploadLimit + 1),
        filename: 'again.mp4',
        onProgress: (sent, total) => reported = sent,
      );
      expect(api.route, [
        'presign',
        'commit',
      ], reason: '内容寻址的最大一笔收益：同一份字节不必再传一次');
      expect(
        reported,
        kRelayUploadLimit + 1,
        reason: '跳过也要报满进度，否则进度条停在 0% 然后凭空消失',
      );
    });

    test('签不出 URL 时给出可执行的错误，而不是回退到必定 413 的中转', () async {
      final api = _BlobApi(presignSupported: false);
      await expectLater(
        uploadAttachment(
          api,
          bytes: _bytes(kRelayUploadLimit + 1),
          filename: 'movie.mp4',
        ),
        throwsA(
          isA<CortexApiException>()
              .having((e) => e.message, 'message', contains('直传'))
              .having((e) => e.message, 'message', contains('32.0 MB')),
        ),
      );
      expect(api.route, ['presign'], reason: '这个文件本来就超了中转上限，退回去只会把清楚的错误换成 413');
    });

    test('嗅探出来的 MIME 决定 kind，而不是文件扩展名', () async {
      final api = _BlobApi();
      final attachment = await uploadAttachment(
        api,
        bytes: _bytes(64),
        filename: 'photo.png',
      );
      expect(attachment.kind, 'image');
      expect(attachment.isImage, isTrue);
    });
  });

  group('附件队列', () {
    test('上传完成后进入待发送列表', () async {
      final container = _boot(_BlobApi());
      addTearDown(container.dispose);
      final queue = container.read(attachmentQueueProvider.notifier);

      await queue.addBytes(
        's1',
        filename: 'a.png',
        bytes: _bytes(512),
        mime: 'image/png',
      );

      final items = queue.forSession('s1');
      expect(items, hasLength(1));
      expect(items.single.status, UploadStatus.ready);
      expect(queue.readyFor('s1'), hasLength(1));
      expect(queue.hasPending('s1'), isFalse);
    });

    test('失败的条目留着字节，可以直接重试', () async {
      var fail = true;
      final container = _boot(_FlakyApi(() => fail));
      addTearDown(container.dispose);
      final queue = container.read(attachmentQueueProvider.notifier);

      await queue.addBytes('s1', filename: 'a.bin', bytes: _bytes(128));
      expect(queue.forSession('s1').single.status, UploadStatus.failed);
      expect(
        queue.readyFor('s1'),
        isEmpty,
        reason: '没登记成功的 hash 绝不能混进 /chat —— 服务端会整轮拒掉',
      );

      fail = false;
      await queue.retry('s1', queue.forSession('s1').single.id);
      expect(queue.forSession('s1').single.status, UploadStatus.ready);
      expect(
        queue.readyFor('s1'),
        hasLength(1),
        reason: 'Web 端没有路径可以重新打开文件，字节必须一直留着',
      );
    });

    test('附件按会话隔离', () async {
      final container = _boot(_BlobApi());
      addTearDown(container.dispose);
      final queue = container.read(attachmentQueueProvider.notifier);

      await queue.addBytes('s1', filename: 'a.png', bytes: _bytes(32));
      await queue.addBytes('s2', filename: 'b.png', bytes: _bytes(32));

      expect(queue.forSession('s1').single.filename, 'a.png');
      expect(queue.forSession('s2').single.filename, 'b.png');

      queue.clear('s1');
      expect(queue.forSession('s1'), isEmpty);
      expect(
        queue.forSession('s2'),
        hasLength(1),
        reason: '发出一个会话不该清掉另一个会话正在准备的附件',
      );
    });

    test('上传中的条目会挡住发送', () async {
      final completer = Completer<void>();
      final container = _boot(_SlowApi(completer.future));
      addTearDown(container.dispose);
      final queue = container.read(attachmentQueueProvider.notifier);

      final pending = queue.addBytes(
        's1',
        filename: 'slow.bin',
        bytes: _bytes(64),
      );
      await _until(() => queue.forSession('s1').isNotEmpty, reason: '条目入队');
      expect(queue.hasPending('s1'), isTrue);

      completer.complete();
      await pending;
      expect(queue.hasPending('s1'), isFalse);
    });

    test('切换数据源会清空队列', () async {
      final container = ProviderContainer(
        overrides: [appConfigProvider.overrideWith(_Config.new)],
      );
      addTearDown(container.dispose);
      final queue = container.read(attachmentQueueProvider.notifier);
      container.listen(
        attachmentQueueProvider,
        (_, _) {},
        fireImmediately: true,
      );

      await queue.addBytes('s1', filename: 'a.png', bytes: _bytes(16));

      // Flipping to the mock swaps the blob store underneath us; every hash we
      // hold refers to content the new backend has never seen.
      container.read(appConfigProvider.notifier).setUseMock(true);
      await Future<void>.delayed(Duration.zero);

      expect(
        queue.forSession('s1'),
        isEmpty,
        reason: '留着旧 hash 会让下一次发送引用一个新后端不认识的 blob',
      );
    });
  });
}

/// Fails until the supplied flag says otherwise.
class _FlakyApi extends _BlobApi {
  _FlakyApi(this.shouldFail);

  final bool Function() shouldFail;

  @override
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async {
    if (shouldFail()) {
      throw const CortexApiException('网络断了', statusCode: null);
    }
    return super.uploadBlob(bytes: bytes, mime: mime, onProgress: onProgress);
  }
}

/// Blocks the upload until the test releases it.
class _SlowApi extends _BlobApi {
  _SlowApi(this.gate);

  final Future<void> gate;

  @override
  Future<BlobRef> uploadBlob({
    required Uint8List bytes,
    String? mime,
    UploadProgress? onProgress,
  }) async {
    await gate;
    return super.uploadBlob(bytes: bytes, mime: mime, onProgress: onProgress);
  }
}
