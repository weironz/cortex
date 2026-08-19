import 'package:cortex_app/core/permission_mode.dart';
import 'dart:async';
import 'package:cortex_app/models/auth_tokens.dart';
import 'package:cortex_app/models/import_plan.dart';
import 'package:cortex_app/import/import_source.dart';
import 'dart:typed_data';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/models/workspace.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/confirm_panel.dart';
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
import 'package:cortex_app/state/confirm_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// A command that is dangerous in its **tail**.
///
/// Deliberately longer than the 60-character per-value cap `compact_args` uses
/// for the tool-row summary: if anyone ever routes the preview through that
/// renderer, or adds a `maxLines` to the preview widget, what disappears is
/// `| sh -s -- --force` — and the user approves the half they could not see.
const _dangerousCommand =
    'command: rm -rf /tmp/build-cache && '
    'curl -sSfL https://example.com/install.sh | sh -s -- --force';

PendingConfirmation _request({
  String token = 'tok-1',
  String? sessionId = 's1',
  Duration remaining = const Duration(seconds: 180),
  String preview = _dangerousCommand,
}) => PendingConfirmation(
  token: token,
  tool: 'shell',
  risk: 'execute',
  preview: preview,
  deadline: DateTime.now().add(remaining),
  sessionId: sessionId,
);

void main() {
  group('confirm 事件的解码', () {
    test('自带做决定所需的全部信息，不依赖前面那条 tool 事件', () {
      final event = ChatEvent.fromJson({
        'type': 'confirm',
        'token': 'abc',
        'tool': 'shell',
        'risk': 'execute',
        'preview': _dangerousCommand,
        'timeout_secs': 180,
      });

      expect(event, isA<ChatConfirmEvent>());
      final request = (event as ChatConfirmEvent).request;
      expect(request.token, 'abc');
      expect(request.tool, 'shell');
      expect(request.risk, 'execute');
      expect(
        request.preview,
        _dangerousCommand,
        reason: '契约明说：Tool 与 Confirm 的先后顺序不保证，所以这条必须自足',
      );
      expect(request.remainingFrom(DateTime.now()).inSeconds, closeTo(180, 2));
    });

    test('倒计时来自时长而不是时间戳，两端时钟不必对齐', () {
      final page = PendingConfirmation.fromJson({
        'token': 'abc',
        'session_id': 's1',
        'tool': 'shell',
        'risk': 'execute',
        'preview': 'x',
        // 故意给一个远在未来的 asked_at：客户端一旦去读它，倒计时就会疯掉
        'asked_at': '2099-01-01T00:00:00Z',
        'expires_in_secs': 30,
      }, now: DateTime(2026));

      expect(page.deadline, DateTime(2026).add(const Duration(seconds: 30)));
    });

    test('expires_in_secs 为 0 就是 0，不会被当成「字段缺失」而回落到 180', () {
      final now = DateTime(2026);
      final about = PendingConfirmation.fromJson({
        'token': 'a',
        'preview': 'x',
        'expires_in_secs': 0,
      }, now: now);
      expect(about.isExpiredAt(now), isTrue);

      final missing = PendingConfirmation.fromJson({
        'token': 'a',
        'preview': 'x',
      }, now: now);
      expect(
        missing.deadline,
        now.add(const Duration(seconds: 180)),
        reason: '缺字段时用 0 会把一条还活着的请求画成已作废并藏起来',
      );
    });

    test('未知 type 仍然安全降级，不至于打断整条流', () {
      expect(
        ChatEvent.fromJson({'type': 'confirm_v2'}),
        isA<ChatUnknownEvent>(),
      );
    });
  });

  group('确认队列', () {
    test('SSE 事件进来后带上会话 id 挂到待办上', () async {
      final container = _boot(_ConfirmApi());
      addTearDown(container.dispose);
      final controller = container.read(confirmControllerProvider.notifier);

      controller.offer(_request(sessionId: null), sessionId: 's7');

      final state = container.read(confirmControllerProvider);
      expect(state.pending, hasLength(1));
      expect(
        state.pending.single.sessionId,
        's7',
        reason:
            'SSE 事件不带 session_id（流本身就属于一个会话），恢复端点带 —— '
            '两条路必须产出同一个对象',
      );
    });

    test('允许后投出回执并标为已允许', () async {
      final api = _ConfirmApi();
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(confirmControllerProvider.notifier);

      controller.offer(_request());
      await controller.answer('tok-1', allow: true);

      expect(api.receipts, [('tok-1', true)]);
      final state = container.read(confirmControllerProvider);
      expect(state.pending, isEmpty);
      expect(state.resolved.single.outcome, ConfirmOutcome.allowed);
      expect(state.error, isNull);
    });

    test('拒绝是一条真的回执，不是等超时', () async {
      final api = _ConfirmApi();
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(confirmControllerProvider.notifier);

      controller.offer(_request());
      await controller.answer('tok-1', allow: false);

      expect(api.receipts, [
        ('tok-1', false),
      ], reason: '两者对 agent 都是拒绝，但立刻答复能马上放掉那一轮占着的连接与上下文');
      expect(
        container.read(confirmControllerProvider).resolved.single.outcome,
        ConfirmOutcome.denied,
      );
    });

    test('晚到的回执拿到 404 —— 这是正常结果，不是错误', () async {
      final api = _ConfirmApi(accept: false);
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(confirmControllerProvider.notifier);

      controller.offer(_request());
      await controller.answer('tok-1', allow: true);

      final state = container.read(confirmControllerProvider);
      expect(
        state.error,
        isNull,
        reason: '别的设备先答了是多端设计**期望**发生的事，报成错误等于把正常行为画红',
      );
      expect(state.pending, isEmpty);
      expect(
        state.resolved.single.outcome,
        ConfirmOutcome.superseded,
        reason: '也不能说成「已允许」—— 用户这一次点击确实没有决定任何事',
      );
    });

    test('真正的传输故障才算错误，且待办留着可以再试', () async {
      final api = _ConfirmApi(throwTransport: true);
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(confirmControllerProvider.notifier);

      controller.offer(_request());
      await controller.answer('tok-1', allow: true);

      final state = container.read(confirmControllerProvider);
      expect(state.error, isNotNull);
      expect(
        state.pending,
        hasLength(1),
        reason: '回执没送到，那一轮还在等；把提示撤掉就等于替用户放弃了',
      );
      expect(state.answering, isEmpty, reason: '按钮必须重新可点');
    });

    test('回执在服务端确认之前不乐观地撤掉提示', () async {
      final api = _ConfirmApi(hold: true);
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(confirmControllerProvider.notifier);

      controller.offer(_request());
      unawaited(controller.answer('tok-1', allow: true));
      await Future<void>.delayed(Duration.zero);

      final mid = container.read(confirmControllerProvider);
      expect(mid.pending, hasLength(1));
      expect(mid.answering, contains('tok-1'));

      api.release(true);
      await Future<void>.delayed(Duration.zero);
      expect(container.read(confirmControllerProvider).pending, isEmpty);
    });
  });

  group('断线重连后的恢复', () {
    test('GET /confirmations 捞回 SSE 断掉时没送到的那条', () async {
      final api = _ConfirmApi(
        outstanding: [_request(token: 'recovered', sessionId: 's3')],
      );
      final container = _boot(api);
      addTearDown(container.dispose);

      await container.read(confirmControllerProvider.notifier).recover();

      final state = container.read(confirmControllerProvider);
      expect(state.pending.single.token, 'recovered');
      expect(
        state.pending.single.sessionId,
        's3',
        reason: 'SSE 流一次性、没有 Last-Event-ID 重放，断了那条请求就再也不会重发',
      );
    });

    test('已经在屏幕上的那条不会被恢复轮询重复出来', () async {
      final api = _ConfirmApi(outstanding: [_request(token: 'tok-1')]);
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(confirmControllerProvider.notifier);

      controller.offer(_request(token: 'tok-1'));
      await controller.recover();

      expect(container.read(confirmControllerProvider).pending, hasLength(1));
    });

    test('恢复轮询不会把倒计时重置回满格', () async {
      // 服务端报「还剩 170 秒」，而手上那条已经走了一会儿只剩 20 秒。
      // 采纳服务端的会让每次重连闪断都把倒计时拨回去，那个数字就不再有意义
      final api = _ConfirmApi(
        outstanding: [
          _request(token: 'tok-1', remaining: const Duration(seconds: 170)),
        ],
      );
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(confirmControllerProvider.notifier);

      controller.offer(
        _request(token: 'tok-1', remaining: const Duration(seconds: 20)),
      );
      await controller.recover();

      expect(
        container
            .read(confirmControllerProvider)
            .pending
            .single
            .remainingFrom(DateTime.now())
            .inSeconds,
        lessThanOrEqualTo(20),
      );
    });

    test('恢复失败是静默的 —— 重连本身就说明网络刚不好过', () async {
      final api = _ConfirmApi(listThrows: true);
      final container = _boot(api);
      addTearDown(container.dispose);

      await container.read(confirmControllerProvider.notifier).recover();
      expect(container.read(confirmControllerProvider).error, isNull);
    });

    test('已经过期的条目不会被恢复成一条点不动的提示', () async {
      final api = _ConfirmApi(
        outstanding: [
          _request(token: 'stale', remaining: const Duration(seconds: -1)),
        ],
      );
      final container = _boot(api);
      addTearDown(container.dispose);

      await container.read(confirmControllerProvider.notifier).recover();
      expect(container.read(confirmControllerProvider).pending, isEmpty);
    });
  });

  group('超时', () {
    // 一个普通 test 而不是 testWidgets：截止时间是拿 `DateTime.now()` 比的，
    // 而 `tester.pump(duration)` 推的是 fake_async 的时钟，它不会动
    // `DateTime.now()`。在 widget 测试里这条永远绿不了 —— 而它守的恰恰是
    // 「静默超时必须被说出来」这件事，绿得没有意义比红更糟
    test('倒计时归零后自己撤下，并说明按拒绝处理', () async {
      final container = _boot(_ConfirmApi());
      addTearDown(container.dispose);
      final controller = container.read(confirmControllerProvider.notifier);

      controller.offer(_request(remaining: const Duration(milliseconds: 150)));
      expect(container.read(confirmControllerProvider).pending, hasLength(1));

      // 真实时间：ticker 是每秒一跳，等两跳足够
      await Future<void>.delayed(const Duration(milliseconds: 2200));

      final state = container.read(confirmControllerProvider);
      expect(state.pending, isEmpty);
      expect(
        state.resolved.single.outcome,
        ConfirmOutcome.expired,
        reason: '服务端超时是**静默**的：不说一声就消失，用户只会以为界面出了 bug',
      );
    });
  });

  // 这一组每条末尾都显式 `container.dispose()`，不用 addTearDown：
  // 待办不空时 `ConfirmController` 有一个每秒的 ticker，而 flutter_test 的
  // 「还有 timer 没清」检查跑在 test body 结束之后、teardown 之前 ——
  // 用 addTearDown 会让每条都以一个和被测行为无关的 assert 失败
  group('弹层', () {
    testWidgets('命令原样显示，一个字都不截', (tester) async {
      final container = _boot(_ConfirmApi());
      container
          .read(confirmControllerProvider.notifier)
          .offer(_request(sessionId: null));

      await tester.pumpWidget(_host(container));
      await tester.pump();

      expect(find.text('需要你确认：shell'), findsOneWidget);
      expect(find.text('执行'), findsOneWidget);

      final preview = tester.widget<SelectableText>(
        find.byWidgetPredicate(
          (w) => w is SelectableText && w.data == _dangerousCommand,
        ),
      );
      expect(
        preview.maxLines,
        isNull,
        reason:
            '服务端已经在 8 KiB 处截过一次并留了显式标记。客户端再截一刀，'
            '被切掉的正好是 `| sh -s -- --force`，而用户批准的就是他没看见的那一半',
      );
      expect(
        preview.style?.fontFamily,
        'monospace',
        reason: '`rm -rf /tmp/x` 与 `rm -rf /tmp /x` 差一个空格，等宽字才看得出来',
      );
      container.dispose();
    });

    testWidgets('倒计时看得见，并写明零点之后会发生什么', (tester) async {
      final container = _boot(_ConfirmApi());
      container
          .read(confirmControllerProvider.notifier)
          // 42.9 而不是 42：`inSeconds` 是向下取整的，构造与渲染之间那几毫秒
          // 会把整 42 秒变成 41
          .offer(_request(remaining: const Duration(milliseconds: 42900)));

      await tester.pumpWidget(_host(container));
      await tester.pump();

      expect(find.textContaining('42s'), findsOneWidget);
      expect(
        find.textContaining('按拒绝处理'),
        findsOneWidget,
        reason: '沉默即拒绝这件事必须写出来 —— 它同时是「走开也是安全的」这个保证',
      );
      container.dispose();
    });

    testWidgets('按钮真的发出回执', (tester) async {
      final api = _ConfirmApi();
      final container = _boot(api);
      container.read(confirmControllerProvider.notifier).offer(_request());

      await tester.pumpWidget(_host(container));
      await tester.pump();

      await tester.tap(find.text('允许执行'));
      // 不 await 数据源调用：mock 的延迟是 Future.delayed，在 widget 测试里
      // 那个 timer 只在 pump 时才走，直接 await 会死锁而不是失败
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 100));

      expect(api.receipts, [('tok-1', true)]);
      container.dispose();
    });

    testWidgets('别的会话的待确认也会显示，并标出它属于谁', (tester) async {
      final container = _boot(_ConfirmApi());
      container
          .read(confirmControllerProvider.notifier)
          .offer(_request(sessionId: '另一个会话'));

      await tester.pumpWidget(_host(container));
      await tester.pump();

      expect(
        find.textContaining('来自另一个会话'),
        findsOneWidget,
        reason:
            '按当前会话过滤会正好藏掉恢复端点存在的那个场景：'
            '另一台设备问出来的、或者重连后捞回来的那条',
      );
      container.dispose();
    });

    testWidgets('没有待确认时整块不占位', (tester) async {
      final container = _boot(_ConfirmApi());

      await tester.pumpWidget(_host(container));
      await tester.pump();

      expect(find.byType(SelectableText), findsNothing);
      container.dispose();
    });
  });
}

ProviderContainer _boot(CortexApi api) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_LiveConfig.new),
    cortexApiProvider.overrideWithValue(api),
  ],
);

Widget _host(ProviderContainer container) => UncontrolledProviderScope(
  container: container,
  child: const MaterialApp(
    home: Scaffold(body: SingleChildScrollView(child: ConfirmPanel())),
  ),
);

class _LiveConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:8080');
}

/// A daemon stand-in for the confirmation surface.
class _ConfirmApi
    with
        ModelSourcesUnsupported,
        AccountUnsupported,
        LocalWorkspaceUnsupported,
        LocalMcpUnsupported,
        RunAttachUnsupported,
        LlmModelsUnsupported,
        SessionSearchUnsupported,
        UsageUnsupported,
        SandboxHealthUnsupported
    implements CortexApi {
  _ConfirmApi({
    this.accept = true,
    this.throwTransport = false,
    this.listThrows = false,
    this.hold = false,
    this.outstanding = const [],
  });

  /// False models the daemon's 404 — the token was already consumed.
  final bool accept;
  final bool throwTransport;
  final bool listThrows;

  /// Keeps the receipt in flight until [release], to check that the prompt is
  /// not retired optimistically.
  final bool hold;

  final List<PendingConfirmation> outstanding;

  final List<(String, bool)> receipts = [];
  Completer<bool>? _held;

  void release(bool accepted) => _held?.complete(accepted);

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
  Future<List<PendingConfirmation>> pendingConfirmations({
    String? sessionId,
  }) async {
    if (listThrows) throw const CortexApiException('网络刚断过');
    return outstanding;
  }

  @override
  Future<bool> answerConfirmation({
    required String token,
    required bool allow,
    String? sessionId,
  }) async {
    receipts.add((token, allow));
    if (throwTransport) {
      throw const CortexApiException('502 Bad Gateway', statusCode: 502);
    }
    if (hold) {
      final completer = _held = Completer<bool>();
      return completer.future;
    }
    return accept;
  }

  @override
  String get label => 'confirm-fake';

  @override
  void dispose() {}

  @override
  Future<HealthStatus> health() async => const HealthStatus(
    status: 'ok',
    version: 't',
    database: 'ok',
    auth: 'token',
  );

  @override
  Future<AuthTicket> issueTicket() => throw UnimplementedError();

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
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    bool sandbox = false,
  }) => const Stream.empty();

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
  Stream<SyncEvent> watchSync() => const Stream.empty();

  @override
  Future<SyncPage> sync({required int since, int limit = 500}) async =>
      SyncPage(cursor: since);
}
