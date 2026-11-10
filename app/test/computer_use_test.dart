/// 电脑操作那个开关。
///
/// 这一组工具**没有围栏** —— 它动的是用户整台机器上正在运行的一切。
/// 所以盯住的四件事全是「默认值与可见性」，而不是功能：
///
/// 1. **默认是关的** —— 而且是「只有明确存过 true 才开」，不是「不是 false
///    就当 true」。后者在设置损坏、键名改过、旧版本没写过时会静默变成开着
/// 2. **agent 说做不到时整节不画** —— 摆一个打开也没用的开关比没有它更糟
/// 3. **关着时那一轮不带这个字段** —— 带了 false 也能跑，但「没开」这件事
///    该由字段的缺席表达
/// 4. **那段说明讲的是「它能看见什么」** —— 用户要做一个知情的决定，
///    而做这个决定需要知道观察只覆盖 allowlist 窗口、窗口内容会交给模型
library;

import 'dart:async';
import 'dart:convert';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/computer_trace_api.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/features/chat/widgets/computer_use_status.dart';
import 'package:cortex_app/features/chat/widgets/computer_use_trace_viewer.dart';
import 'package:cortex_app/features/settings/pages/computer_use_section.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/models/account.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/models/pending_confirmation.dart';
import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/chat_state.dart';
import 'package:cortex_app/state/confirm_controller.dart';
import 'package:cortex_app/state/model_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');

  void changeBaseForTest(String value) =>
      state = state.copyWith(baseUrl: value);
}

class _TraceIdentityAuth extends AuthController {
  @override
  AuthState build() => const AuthState(phase: AuthPhase.ready, token: 'test-a');

  void changeForTest(String value) => state = state.copyWith(token: value);
}

class _UnprovenTraceApi implements CortexApi, ComputerTraceApi {
  int calls = 0;

  @override
  Future<ComputerUseTraceHistory> computerUseTraces(String sessionId) async {
    calls++;
    return const ComputerUseTraceHistory(traces: []);
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _StaticTraceChat extends ChatController {
  @override
  ChatState build() => const ChatState(activeSessionId: 'ses_trace_a');
}

class _EmptyTraceConfirm extends ConfirmController {
  @override
  ConfirmState build() => const ConfirmState();
}

HttpCortexApi _httpTraceTestApi(
  Future<http.Response> Function() trace, {
  String baseUrl = 'http://127.0.0.1:8080',
  String? frontsDeployment,
}) => HttpCortexApi(
  baseUrl: baseUrl,
  frontsDeployment: frontsDeployment,
  client: MockClient((request) async {
    if (request.url.path.startsWith('/local/computer-traces/')) return trace();
    if (request.url.path == '/sessions') {
      return http.Response('{"sessions":[]}', 200);
    }
    return http.Response('{"error":"not found","retryable":false}', 404);
  }),
);

/// 记下 `/chat` 那一轮到底准没准操作电脑。
class _SpyApi extends MockCortexApi {
  _SpyApi() : super(instant: true);

  bool? sent;

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    Assistant? assistant,
    List<Skill> skills = const [],
    bool computerUse = false,
    ImagePrefs? imagePrefs,
  }) {
    sent = computerUse;
    return super.chat(
      sessionId: sessionId,
      message: message,
      attachments: attachments,
      permissionMode: permissionMode,
      model: model,
      source: source,
      assistant: assistant,
      skills: skills,
      computerUse: computerUse,
      imagePrefs: imagePrefs,
    );
  }
}

/// 让测试在发送后逐条推进工具事件，而不是等一条预先写好的完整流。
class _StreamingApi extends MockCortexApi {
  _StreamingApi() : super(instant: true);

  final events = StreamController<ChatEvent>();

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    Assistant? assistant,
    List<Skill> skills = const [],
    bool computerUse = false,
    ImagePrefs? imagePrefs,
  }) => events.stream;
}

class _TraceApi extends _StreamingApi implements ComputerTraceApi {
  _TraceApi(this.history);

  ComputerUseTraceHistory history;
  int calls = 0;
  final queriedSessionIds = <String>[];

  @override
  Future<ComputerUseTraceHistory> computerUseTraces(String sessionId) async {
    calls += 1;
    queriedSessionIds.add(sessionId);
    return history;
  }
}

class _MissingTraceApi extends _StreamingApi implements ComputerTraceApi {
  int calls = 0;

  @override
  Future<ComputerUseTraceHistory> computerUseTraces(String sessionId) async {
    calls++;
    throw const CortexApiException('没有历史端点', statusCode: 404);
  }
}

class _IncompleteTraceApi extends _StreamingApi implements ComputerTraceApi {
  int calls = 0;

  @override
  Future<ComputerUseTraceHistory> computerUseTraces(String sessionId) async {
    calls++;
    throw const CortexApiException(
      '该会话 Computer Use 历史不完整；请人工核对',
      statusCode: 409,
    );
  }
}

class _ChangingTraceApi extends _StreamingApi implements ComputerTraceApi {
  _ChangingTraceApi(this.history);

  ComputerUseTraceHistory history;
  CortexApiException? failure;
  Future<ComputerUseTraceHistory>? pending;

  @override
  Future<ComputerUseTraceHistory> computerUseTraces(String sessionId) async {
    if (pending case final future?) return future;
    if (failure case final error?) throw error;
    return history;
  }
}

class _TraceRequest {
  _TraceRequest(this.sessionId);

  final String sessionId;
  final result = Completer<ComputerUseTraceHistory>();
}

/// 请求由测试独立结算，才能复现同一会话的旧请求迟到。
class _ScriptedTraceApi extends _StreamingApi implements ComputerTraceApi {
  final requests = <_TraceRequest>[];

  @override
  Future<ComputerUseTraceHistory> computerUseTraces(String sessionId) {
    final request = _TraceRequest(sessionId);
    requests.add(request);
    return request.result.future;
  }
}

/// 聊天仍用测试替身，trace 走真实 HTTP 客户端的解码和错误映射。
class _HttpTraceApi extends _StreamingApi
    implements ComputerTraceApi, AbortableComputerTraceApi {
  _HttpTraceApi(this.httpApi);

  final HttpCortexApi httpApi;

  @override
  Future<ComputerUseTraceHistory> computerUseTraces(String sessionId) =>
      httpApi.computerUseTraces(sessionId);

  @override
  Future<ComputerUseTraceHistory> computerUseTracesAbortable(
    String sessionId, {
    required Future<void> abortTrigger,
  }) =>
      httpApi.computerUseTracesAbortable(sessionId, abortTrigger: abortTrigger);
}

ProviderContainer _boot({
  CortexApi? api,
  CortexApi Function()? apiFactory,
  Future<Account?> Function()? identityLookup,
  Map<String, String> settings = const {},
  bool agentCanDoIt = true,
  ComputerCapabilities? capabilities,
  bool? modelVision,
  String role = HealthStatus.roleLocalAgent,
}) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    if (identityLookup != null)
      authControllerProvider.overrideWith(_TraceIdentityAuth.new),
    if (identityLookup != null)
      authProbeApiProvider.overrideWith((ref) {
        final baseUrl = ref.watch(appConfigProvider).baseUrl;
        return (_) => HttpCortexApi(
          baseUrl: baseUrl,
          client: MockClient((request) async {
            if (request.url.path != '/auth/me') throw StateError('身份请求路径错误');
            final account = await identityLookup();
            if (account == null) return http.Response('{}', 404);
            return http.Response(
              jsonEncode({
                'user_id': account.userId,
                'username': account.username,
                'schema_name': account.schemaName,
              }),
              200,
              headers: const {
                'content-type': 'application/json; charset=utf-8',
              },
            );
          }),
        );
      }),
    if (apiFactory != null)
      cortexApiProvider.overrideWith((ref) {
        if (identityLookup != null) {
          ref.watch(authControllerProvider.select((state) => state.token));
        }
        return apiFactory();
      })
    else
      cortexApiProvider.overrideWithValue(api ?? MockCortexApi(instant: true)),
    settingsReaderProvider.overrideWithValue(() async => settings),
    settingsWriterProvider.overrideWithValue((_) async {}),
    healthProvider.overrideWith(
      (_) async => HealthStatus(
        status: 'ok',
        version: '0.1',
        database: 'unknown',
        role: role,
        computerUse: agentCanDoIt,
        computerCapabilities: capabilities,
      ),
    ),
    if (modelVision != null)
      selectedModelVisionProvider.overrideWithValue(modelVision),
    if (settings['computer_use'] == 'true')
      computerUseProvider.overrideWith(_EnabledComputerUse.new),
  ],
);

/// widget 测试只需要渲染「已开启」的只读状态；设置恢复本身已经有纯 Dart
/// 用例覆盖。直接在 build 里给 true，避免异步 microtask 在 Flutter 测试
/// 的事件循环里挂住。
class _EnabledComputerUse extends ComputerUseNotifier {
  @override
  bool build() => true;
}

Future<void> _pump(WidgetTester tester, ProviderContainer c) =>
    tester.pumpWidget(
      UncontrolledProviderScope(
        container: c,
        child: const MaterialApp(
          home: Scaffold(
            body: SingleChildScrollView(child: ComputerUseSection()),
          ),
        ),
      ),
    );

Future<void> _pumpStatus(WidgetTester tester, ProviderContainer c) =>
    tester.pumpWidget(
      UncontrolledProviderScope(
        container: c,
        child: const MaterialApp(home: Scaffold(body: ComputerUseStatus())),
      ),
    );

Future<void> _pumpTraceButton(WidgetTester tester, ProviderContainer c) =>
    tester.pumpWidget(
      UncontrolledProviderScope(
        container: c,
        child: const MaterialApp(
          home: Scaffold(body: Center(child: ComputerUseTraceButton())),
        ),
      ),
    );

Future<ProviderContainer> _startTraceProbe(
  WidgetTester tester,
  _ScriptedTraceApi api,
) async {
  final c = _boot(api: api);
  addTearDown(c.dispose);
  await _pumpTraceButton(tester, c);
  c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
  await _tick(tester);
  expect(api.requests.map((request) => request.sessionId), ['ses_trace_a']);
  return c;
}

Future<ProviderContainer> _startComputerTurn(
  WidgetTester tester,
  _StreamingApi api,
) async {
  final c = _boot(api: api);
  addTearDown(c.dispose);
  addTearDown(api.events.close);
  await _pumpStatus(tester, c);
  expect(
    await c.read(chatControllerProvider.notifier).send('操作一下窗口'),
    isTrue,
    reason: '测试流必须先成功挂到一轮正在生成的回答上',
  );
  await _tick(tester, 3);
  return c;
}

/// 把「开机读设置」那件事跑完。
///
/// ⚠️ **要先 `read` 一次再等。** provider 是懒建的：不先读的话 `build()`
/// 根本没跑，`_restore` 那个 microtask 也就没排上队 —— 于是等多久都还是默认值，
/// 而测试红在「明确开过的没记住」上，与真正的原因毫无关系。
Future<bool> _settled(ProviderContainer c) async {
  c.read(computerUseProvider);
  for (var i = 0; i < 10; i++) {
    await Future<void>.delayed(Duration.zero);
  }
  return c.read(computerUseProvider);
}

Future<List<String>> _settledAllowlist(ProviderContainer c) async {
  c.read(computerUseAllowlistProvider);
  for (var i = 0; i < 10; i++) {
    await Future<void>.delayed(Duration.zero);
  }
  return c.read(computerUseAllowlistProvider);
}

Future<List<String>> _settledBrowserAllowlist(ProviderContainer c) async {
  c.read(computerBrowserAllowlistProvider);
  for (var i = 0; i < 10; i++) {
    await Future<void>.delayed(Duration.zero);
  }
  return c.read(computerBrowserAllowlistProvider);
}

Future<List<String>> _settledPackagedAllowlist(ProviderContainer c) async {
  c.read(computerPackagedAllowlistProvider);
  for (var i = 0; i < 10; i++) {
    await Future<void>.delayed(Duration.zero);
  }
  return c.read(computerPackagedAllowlistProvider);
}

Future<void> _tick(WidgetTester tester, [int n = 12]) async {
  for (var i = 0; i < n; i++) {
    await tester.pump(const Duration(milliseconds: 20));
  }
}

Map<String, dynamic> _traceHistoryPayload() => {
  'traces': [
    {
      'schema_version': 1,
      'run_id': 'run-history-1',
      'session_id': 'ses_01JQZ8K3M9',
      'policy_version': 'cua-windows-v1',
      'app': {
        'kind': 'windows_executable',
        'stable_id': 'publisher/product',
        'publisher_verified': true,
      },
      'redaction': {
        'raw_screenshot_frames': 'hash_only',
        'typed_text': 'omitted',
        'ax_secure_values': 'omitted',
        'cookies_tokens_passwords': 'omitted',
        'clipboard_contents': 'omitted',
      },
      'events': [
        {
          'sequence': 1,
          'type': 'observation',
          'observation': {
            'frame_hash': 'frame-before',
            'window_ref': 'window-1',
            'accessibility_summary_hash': 'ax-summary-hash',
            'screenshot_hash': 'screenshot-hash',
          },
        },
        {
          'sequence': 2,
          'type': 'policy_decision',
          'action': {
            'kind': 'type_text',
            'target_ref': 'element-1',
            'text': {'redacted': true},
          },
          'decision': 'allow',
        },
        {
          'sequence': 3,
          'type': 'confirmation',
          'confirmation': {
            'preview_hash': 'preview-hash',
            'decision': 'approved',
          },
        },
        {
          'sequence': 4,
          'type': 'action',
          'action_id': 7,
          'summary': {
            'kind': 'type_text',
            'target_ref': 'element-1',
            'text': {'redacted': true},
          },
        },
        {
          'sequence': 5,
          'type': 'receipt',
          'action_id': 7,
          'receipt': {
            'receipt_id': 'receipt-7',
            'status': 'completed',
            'duration_ms': 31,
            'failure': null,
          },
        },
        {
          'sequence': 6,
          'type': 'verification',
          'verification': {
            'action_id': 7,
            'check': {'kind': 'element_present', 'target_ref': 'element-2'},
            'post_observation': {
              'frame_hash': 'frame-after',
              'window_ref': 'window-1',
              'accessibility_summary_hash': 'ax-after',
              'screenshot_hash': null,
            },
            'result': {'status': 'passed'},
          },
        },
        {'sequence': 7, 'type': 'aborted', 'reason': 'user_cancelled'},
      ],
      'outcome': 'aborted',
    },
  ],
};

Map<String, dynamic> _denialTraceHistoryPayload({
  Object? tool = 'computer_browser_open',
  Object? sequence = 1,
  Object? timestamp = 1700000000123,
}) => {
  'traces': [
    {
      'schema_version': 2,
      'run_id': 'run-denied-1',
      'session_id': 'ses_01JQZ8K3M9',
      'policy_version': 'cua-windows-v1',
      'app': null,
      'redaction': {
        'raw_screenshot_frames': 'omitted',
        'typed_text': 'omitted',
        'ax_secure_values': 'omitted',
        'cookies_tokens_passwords': 'omitted',
        'clipboard_contents': 'omitted',
      },
      'events': [
        {
          'sequence': sequence,
          'type': 'pre_dispatch_rejection',
          'tool': tool,
          'timestamp_unix_ms': timestamp,
        },
      ],
      'outcome': 'aborted',
    },
  ],
};

void main() {
  test('细粒度能力不会被旧 computer_use 布尔夸大', () {
    final health = HealthStatus.fromJson(const {
      'status': 'ok',
      'version': 'test',
      'database': 'unknown',
      'role': HealthStatus.roleLocalAgent,
      'computer_use': true,
      'computer_capabilities': {
        'list_apps': true,
        'list_windows': false,
        'observe': true,
        'act': false,
        'browser': false,
        'background_input': false,
        'secure_input_detection': true,
      },
    });

    expect(health.computerCapabilities?.listApps, isTrue);
    expect(health.computerCapabilities?.listWindows, isFalse);
    expect(health.canObserveComputer, isTrue);
    expect(
      health.canActOnComputer,
      isFalse,
      reason: 'observe 为 true 不能自动把 act 升格为 true',
    );
    expect(health.computerCapabilities?.browser, isFalse);
    expect(health.computerCapabilities?.backgroundInput, isFalse);
    expect(health.computerCapabilities?.secureInputDetection, isTrue);
  });

  test('旧 agent 没有细粒度字段时才回退到旧布尔', () {
    final legacy = HealthStatus.fromJson(const {
      'status': 'ok',
      'version': 'test',
      'database': 'unknown',
      'role': HealthStatus.roleLocalAgent,
      'computer_use': true,
    });
    expect(legacy.computerCapabilities, isNull);
    expect(legacy.canObserveComputer, isTrue);
    expect(legacy.canActOnComputer, isTrue);

    final malformed = HealthStatus.fromJson(const {
      'status': 'ok',
      'version': 'test',
      'database': 'unknown',
      'role': HealthStatus.roleLocalAgent,
      'computer_use': true,
      'computer_capabilities': 'yes',
    });
    expect(
      malformed.computerCapabilities,
      isNull,
      reason: '形状不对时按旧 agent 处理，不能把字符串猜成能力',
    );
  });

  test('写入 readiness 按 snake_case 解析，不能替代 act 或整体能力', () {
    const states = {
      'ready': ComputerWriteReadiness.ready,
      'authority_missing': ComputerWriteReadiness.authorityMissing,
      'provision_incomplete': ComputerWriteReadiness.provisionIncomplete,
      'owner_mismatch': ComputerWriteReadiness.ownerMismatch,
      'authority_unresolved': ComputerWriteReadiness.authorityUnresolved,
      'authority_invalid': ComputerWriteReadiness.authorityInvalid,
      'authority_busy': ComputerWriteReadiness.authorityBusy,
      'probe_unavailable': ComputerWriteReadiness.probeUnavailable,
      'unsupported': ComputerWriteReadiness.unsupported,
    };
    for (final entry in states.entries) {
      final health = HealthStatus.fromJson({
        'computer_use': true,
        'computer_capabilities': {
          'observe': true,
          'act': true,
          'browser': true,
          'write_readiness': entry.key,
        },
      });
      expect(health.computerCapabilities?.writeReadiness, entry.value);
      expect(health.canObserveComputer, isTrue, reason: '写入状态不能撤销独立观察能力');
      expect(
        health.canActOnComputer,
        entry.value == ComputerWriteReadiness.ready,
        reason: '${entry.key} 必须限制 act/browser 的原始自报',
      );
    }
    for (final computerUse in [false, true]) {
      final health = HealthStatus.fromJson({
        'computer_use': computerUse,
        'computer_capabilities': {'act': false, 'write_readiness': 'ready'},
      });
      expect(
        health.canActOnComputer,
        isFalse,
        reason: 'ready 不能将 act=false 升格',
      );
    }
    expect(
      HealthStatus.fromJson({
        'computer_use': false,
        'computer_capabilities': {'act': true, 'write_readiness': 'ready'},
      }).canActOnComputer,
      isFalse,
      reason: 'ready 也不能越过整体 computer_use=false',
    );
  });

  test('readiness 缺字段兼容旧 host，显式 null 或畸形值均禁止写入', () {
    final legacy = ComputerCapabilities.fromJson({'act': true});
    expect(legacy.writeReadiness, isNull);
    expect(legacy.canAct, isTrue, reason: '缺字段沿用旧版 act 声明');
    for (final value in <Object?>[
      null,
      'future_state',
      '',
      'READY',
      ' ready ',
      true,
      false,
      1,
      ['ready'],
      {'state': 'ready'},
    ]) {
      final health = HealthStatus.fromJson({
        'computer_use': true,
        'computer_capabilities': {
          'observe': true,
          'act': true,
          'browser': true,
          'write_readiness': value,
        },
      });
      expect(
        health.computerCapabilities?.writeReadiness,
        ComputerWriteReadiness.unknown,
        reason: '显式 $value 不能被当成字段缺失或 ready',
      );
      expect(health.canActOnComputer, isFalse, reason: '无效状态不能被 act=true 绕过');
      expect(health.canObserveComputer, isTrue);
    }
  });

  test('默认关；只有明确存过 true 才开', () async {
    final fresh = _boot();
    addTearDown(fresh.dispose);
    expect(fresh.read(computerUseProvider), isFalse, reason: '这一组没有围栏，默认必须是关的');

    // ⚠️ 一份**没有**这一项的设置（旧版本、或者刚装好）要读成关
    final legacy = _boot(settings: const {'permission_mode': 'ask'});
    addTearDown(legacy.dispose);
    expect(
      await _settled(legacy),
      isFalse,
      reason: '设置里没有这一项时必须是关的 —— 「不是 false 就当 true」会让旧安装静默开着',
    );

    // 值是别的字符串（文件被手改坏了）也要读成关
    final broken = _boot(settings: const {'computer_use': 'yes'});
    addTearDown(broken.dispose);
    expect(
      await _settled(broken),
      isFalse,
      reason: '只认字面的 true。认别的写法等于给一个坏掉的设置文件开门',
    );

    final on = _boot(settings: const {'computer_use': 'true'});
    addTearDown(on.dispose);
    expect(await _settled(on), isTrue, reason: '明确开过的要记住');
  });

  test('关着的时候那一轮不带这个字段', () async {
    final api = _SpyApi();
    final c = _boot(api: api);
    addTearDown(c.dispose);

    await c.read(chatControllerProvider.notifier).send('你好');
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }
    expect(api.sent, isFalse, reason: '没开就不能带 —— 服务端只看这一个字段决定摆不摆那组工具');

    // ⚠️ 换一个容器再测「开着」那一半：同一条会话里上一轮还在飞时，
    // `send` 会被那道「一轮只跑一次」的闸挡掉 —— 而挡掉的表现是
    // `api.sent` 停在上一轮的值上，读起来像「开关没生效」
    final onApi = _SpyApi();
    final onC = _boot(
      api: onApi,
      settings: const {'computer_use': 'true'},
      modelVision: true,
    );
    addTearDown(onC.dispose);
    expect(await _settled(onC), isTrue);

    await onC.read(chatControllerProvider.notifier).send('看看屏幕');
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }
    expect(
      onApi.sent,
      isTrue,
      // 开了却不带的表现是「我明明打开了它却说看不见屏幕」
      reason: '开了就得带上，否则这个开关是个摆设',
    );
  });

  test('模型看不懂图且后端未确认结构化通道时，开着也不发', () async {
    final api = _SpyApi();
    final c = _boot(api: api, settings: const {'computer_use': 'true'});
    addTearDown(c.dispose);
    expect(await _settled(c), isTrue, reason: '开关本身是开着的');
    // 拉回目录来验证已知不支持视觉的分支；未知模型的兼容边界另有覆盖。
    //
    // ⚠️ 用 `listen` 而不是 `read`：这条链上是 `autoDispose`，光 read 一下
    // 会在下一次事件循环里被回收，于是永远停在「还没拉到」
    final keep = c.listen(modelCatalogProvider, (_, _) {});
    addTearDown(keep.close);
    for (var i = 0; i < 30; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    // mock 目录里的默认模型看不懂图。⚠️ 截图是在**工具执行时**才拍的，
    // 所以「发过去、模型回 400」的代价不是一条报错，而是**屏幕白拍一次
    // 并且已经发给供应商了**。实测：DeepSeek 那几个都会回
    // `This model does not support image`
    expect(
      c.read(selectedModelVisionProvider),
      isFalse,
      reason: '这条测试的前提是当前模型看不懂图；前提不成立的话下面那条断言没有意义',
    );
    await c.read(chatControllerProvider.notifier).send('看看屏幕');
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }
    expect(
      api.sent,
      isFalse,
      reason: '模型看不懂图时还带这个字段，等于让它把屏幕拍下来发出去，然后收到一条看不懂的 400',
    );
  });

  testWidgets('agent 说做不到时，整节不画', (tester) async {
    final c = _boot(agentCanDoIt: false, role: HealthStatus.roleCortexd);
    addTearDown(c.dispose);

    await _pump(tester, c);
    await _tick(tester);
    expect(
      find.byKey(const ValueKey('computer_use:toggle')),
      findsNothing,
      // 做不到有两种：跑在容器里（没有屏幕）、或这个构建没编进那一组。
      // 两种都不是「关着」而是「没有」—— 一个打开也没用的开关比没有它更糟
      reason: '做不到时摆一个开关，用户打开它、然后发现什么也没发生，且无处可查',
    );
  });

  testWidgets('本机驱动未就绪时保留配置入口，而不是把设置页藏掉', (tester) async {
    final c = _boot(agentCanDoIt: false);
    addTearDown(c.dispose);

    await _pump(tester, c);
    await _tick(tester);

    expect(
      find.byKey(const ValueKey('computer_use:not-ready')),
      findsOneWidget,
      reason: '本机 allowlist 为空时用户仍需要一个入口修复配置',
    );
    expect(
      find.byKey(const ValueKey('computer_use:allowlist-add')),
      findsOneWidget,
    );
    final toggle = tester.widget<Switch>(
      find.byKey(const ValueKey('computer_use:toggle')),
    );
    expect(toggle.onChanged, isNull, reason: '驱动未就绪时不能假装开关可用');
  });

  testWidgets('做得到时，页面说明窗口范围、动作边界和 handoff', (tester) async {
    final c = _boot(
      settings: const {'computer_use': 'true'},
      capabilities: const ComputerCapabilities(
        listApps: true,
        listWindows: true,
        observe: true,
        act: true,
        browser: false,
        backgroundInput: true,
        secureInputDetection: true,
      ),
    );
    addTearDown(c.dispose);

    await _pump(tester, c);
    await _tick(tester);
    expect(find.byKey(const ValueKey('computer_use:toggle')), findsOneWidget);
    expect(
      find.textContaining('Windows 本机可操作 allowlist 内的桌面窗口；未配置站点时不启动隔离浏览器。'),
      findsOneWidget,
      reason: '用户需要知道当前生产路径以 Windows 为主',
    );
    expect(
      find.textContaining('每次观察绑定一个 allowlist 内的应用窗口'),
      findsOneWidget,
      reason: '观察范围必须与按窗口、按 allowlist 的后端实现一致',
    );
    expect(
      find.textContaining('未列入 allowlist 的应用不会出现在窗口列表里'),
      findsOneWidget,
      reason: '不能让用户以为 agent 能看见或操作整台机器上的所有窗口',
    );
    expect(
      find.textContaining('点击和输入落在真实的桌面窗口上，不在沙箱里'),
      findsOneWidget,
      reason: 'act=true 时必须说明动作会离开文件沙箱',
    );
    expect(
      find.textContaining('密码和凭据输入、付款确认、系统权限与 UAC 弹窗'),
      findsOneWidget,
      reason: '敏感字段、付款和系统弹窗必须明确交回给用户',
    );
    expect(
      find.textContaining('每次询问'),
      findsOneWidget,
      reason: 'act=true 时要告诉用户如何逐次查看动作审批',
    );
    expect(
      find.textContaining('屏幕上的一切'),
      findsNothing,
      reason: '新实现按 allowlist 窗口观察，不能继续声称截图覆盖整块屏幕',
    );
  });

  testWidgets('细粒度能力逐项按 /health 展示', (tester) async {
    final c = _boot(
      capabilities: const ComputerCapabilities(
        listApps: true,
        listWindows: true,
        observe: true,
        act: false,
        browser: false,
        backgroundInput: true,
        secureInputDetection: false,
      ),
    );
    addTearDown(c.dispose);

    await _pump(tester, c);
    await _tick(tester);

    expect(find.text('应用列表：可用'), findsOneWidget);
    expect(find.text('窗口列表：可用'), findsOneWidget);
    expect(find.text('界面观察：可用'), findsOneWidget);
    expect(find.text('输入动作：不可用'), findsOneWidget);
    expect(find.text('后台输入：可用'), findsOneWidget);
    expect(find.text('敏感字段检测：不可用'), findsOneWidget);
    expect(find.text('浏览器自动化：不可用'), findsOneWidget);
  });

  for (final entry in const {
    'authority_missing': '写入尚未初始化',
    'provision_incomplete': '写入初始化尚未完成',
    'owner_mismatch': '写入记录属于其他用户',
    'authority_unresolved': '写入状态未决',
    'authority_invalid': '写入记录无效或已损坏',
    'authority_busy': '写入状态被其他进程占用',
    'probe_unavailable': '暂时无法检查写入状态',
    'unsupported': '当前宿主不支持电脑输入动作',
    'future_state': '无法识别写入状态',
  }.entries) {
    testWidgets('写入状态 ${entry.key} 显示中文原因并保留只读观察', (tester) async {
      final c = _boot(
        settings: const {'computer_use': 'true'},
        modelVision: true,
        capabilities: ComputerCapabilities.fromJson({
          'list_windows': true,
          'observe': true,
          'act': true,
          'browser': true,
          'background_input': true,
          'write_readiness': entry.key,
        }),
      );
      addTearDown(c.dispose);
      await _pump(tester, c);
      await _tick(tester);

      expect(c.read(computerUseAvailableProvider), isTrue);
      expect(c.read(computerActAvailableProvider), isFalse);
      final note = tester.widget<Text>(
        find.byKey(const ValueKey('computer_use:write-readiness')),
      );
      expect(note.data, contains(entry.value));
      expect(note.data, contains('仍可观察已允许的窗口'));
      expect(find.text('界面观察：可用'), findsOneWidget);
      expect(find.text('输入动作：不可用'), findsOneWidget);
      expect(find.text('后台输入：不可用'), findsOneWidget);
      expect(find.text('浏览器自动化：不可用'), findsOneWidget);
      expect(find.textContaining('已开启只读。'), findsOneWidget);
      expect(find.textContaining('Windows 本机可操作 allowlist'), findsNothing);
      expect(tester.takeException(), isNull, reason: '状态说明不能破坏既有布局');
    });
  }

  testWidgets('显式 null readiness 不按旧 host 放行，也不声称可观察', (tester) async {
    final c = _boot(
      capabilities: ComputerCapabilities.fromJson({
        'act': true,
        'browser': true,
        'write_readiness': null,
      }),
    );
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _tick(tester);

    expect(c.read(computerActAvailableProvider), isFalse);
    final note = tester.widget<Text>(
      find.byKey(const ValueKey('computer_use:write-readiness')),
    );
    expect(note.data, contains('无法识别写入状态'));
    expect(note.data, isNot(contains('仍可观察')));
    expect(find.text('输入动作：不可用'), findsOneWidget);
    expect(find.text('浏览器自动化：不可用'), findsOneWidget);
  });

  for (final act in [false, true]) {
    testWidgets('ready 的摘要仍遵守 act=$act', (tester) async {
      final c = _boot(
        capabilities: ComputerCapabilities.fromJson({
          'observe': true,
          'act': act,
          'browser': true,
          'background_input': true,
          'write_readiness': 'ready',
        }),
      );
      addTearDown(c.dispose);
      await _pump(tester, c);
      await _tick(tester);

      expect(c.read(computerActAvailableProvider), act);
      expect(find.textContaining('写入前置状态已就绪'), findsOneWidget);
      expect(find.text('输入动作：${act ? '可用' : '不可用'}'), findsOneWidget);
      expect(find.text('后台输入：${act ? '可用' : '不可用'}'), findsOneWidget);
      expect(find.text('浏览器自动化：${act ? '可用' : '不可用'}'), findsOneWidget);
    });
  }

  testWidgets('缺 readiness 的旧 host 保留动作声明且不宣称初始化已完成', (tester) async {
    final c = _boot(
      capabilities: const ComputerCapabilities(observe: true, act: true),
    );
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _tick(tester);

    expect(c.read(computerActAvailableProvider), isTrue);
    expect(find.text('输入动作：可用'), findsOneWidget);
    expect(find.textContaining('写入初始化状态未报告'), findsOneWidget);
    expect(find.textContaining('写入前置状态已就绪'), findsNothing);
  });

  testWidgets('旧 agent 未报告的细项不会被猜成不可用', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);

    await _pump(tester, c);
    await _tick(tester);

    expect(
      find.textContaining('对端是旧版 agent，没有报告窗口列表、后台输入等细项'),
      findsOneWidget,
      reason: '“没报告”与“明确不支持”必须分开，否则旧 agent 会被画成能力故障',
    );
    expect(find.text('窗口列表：不可用'), findsNothing);
  });

  testWidgets('observe-only 能力不会在设置页冒充鼠标键盘操作', (tester) async {
    final c = _boot(
      settings: const {'computer_use': 'true'},
      capabilities: const ComputerCapabilities(
        listApps: false,
        listWindows: true,
        observe: true,
        act: false,
        browser: false,
        backgroundInput: false,
        secureInputDetection: false,
      ),
      modelVision: true,
    );
    addTearDown(c.dispose);
    expect(c.read(computerUseProvider), isTrue, reason: '这条测试需要从「明确开启」的只读态开始');

    await _pump(tester, c);
    await _tick(tester);

    expect(find.byKey(const ValueKey('computer_use:toggle')), findsOneWidget);
    expect(find.textContaining('当前版本只开放观察'), findsOneWidget);
    expect(find.textContaining('已开启只读。'), findsOneWidget);
    expect(
      find.textContaining('当前只开放窗口与界面元素观察'),
      findsOneWidget,
      reason: 'act=false 时不能继续展示“点击和输入落在真实窗口”的动作告知',
    );
    expect(find.text('输入动作：不可用'), findsOneWidget);
    expect(find.textContaining('点击和输入落在真实的窗口上'), findsNothing);
  });

  testWidgets('模型明确不支持视觉时，在开关旁显示错误', (tester) async {
    final c = _boot(
      settings: const {'computer_use': 'true'},
      capabilities: const ComputerCapabilities(
        listApps: true,
        listWindows: true,
        observe: true,
        act: true,
        backgroundInput: true,
        secureInputDetection: true,
      ),
      modelVision: false,
    );
    addTearDown(c.dispose);

    await _pump(tester, c);
    await _tick(tester);

    expect(
      find.byKey(const ValueKey('computer_use:vision-error')),
      findsOneWidget,
    );
    expect(
      find.textContaining('当前模型明确不支持视觉输入'),
      findsWidgets,
      reason: '开关开着但模型看不见图时，必须当场显示错误而不是等请求返回 400',
    );
  });

  testWidgets('computer_observe 出现只读状态，结果到达后立即消失', (tester) async {
    final api = _StreamingApi();
    await _startComputerTurn(tester, api);

    api.events.add(
      const ChatToolEvent(
        name: 'computer_observe',
        summary: '调用 computer_observe',
        phase: ToolPhase.call,
      ),
    );
    await _tick(tester, 3);

    expect(
      find.byKey(const ValueKey('computer_use:status')),
      findsOneWidget,
      reason: '观察工具开始执行时必须让用户知道 Cortex 正在读取本机窗口',
    );
    expect(find.text('正在观察本机窗口'), findsOneWidget);
    expect(find.text('只读'), findsOneWidget);
    expect(find.text('观察窗口内容'), findsOneWidget);
    expect(find.textContaining('allowlist'), findsOneWidget);
    expect(find.textContaining('输入框右侧的停止键'), findsOneWidget);

    api.events.add(
      const ChatToolEvent(
        name: 'computer_observe',
        summary: 'computer_observe 返回 1 个窗口',
      ),
    );
    await _tick(tester, 3);

    expect(
      find.byKey(const ValueKey('computer_use:status')),
      findsNothing,
      reason: '这次工具已经有结果，继续挂“正在使用”会把已完成说成进行中',
    );

    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);
  });

  testWidgets('run 结束时即使没有工具结果，运行态也会消失', (tester) async {
    final api = _StreamingApi();
    await _startComputerTurn(tester, api);

    api.events.add(
      const ChatToolEvent(
        name: 'computer_wait',
        summary: '调用 computer_wait',
        phase: ToolPhase.call,
      ),
    );
    await _tick(tester, 3);
    expect(find.text('等待窗口达到指定状态'), findsOneWidget);

    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);

    expect(
      find.byKey(const ValueKey('computer_use:status')),
      findsNothing,
      reason: '终态到达后 streaming turn 已撤掉，状态条不能残留成幽灵提示',
    );
  });

  testWidgets('所有电脑工具都有准确文案，并区分只读和写操作', (tester) async {
    final api = _StreamingApi();
    await _startComputerTurn(tester, api);

    const cases = <({String name, String title, String badge, String action})>[
      (name: 'computer_list', title: '正在观察本机窗口', badge: '只读', action: '列出允许窗口'),
      (
        name: 'computer_observe',
        title: '正在观察本机窗口',
        badge: '只读',
        action: '观察窗口内容',
      ),
      (
        name: 'computer_wait',
        title: '正在观察本机窗口',
        badge: '只读',
        action: '等待窗口达到指定状态',
      ),
      (
        name: 'computer_act',
        title: '正在操作本机窗口',
        badge: '会改变状态',
        action: '执行窗口动作',
      ),
      (
        name: 'computer_browser_open',
        title: '正在启动隔离浏览器',
        badge: '隔离 profile',
        action: '创建一次性浏览器会话',
      ),
      (
        name: 'computer_browser_observe',
        title: '正在读取隔离网页',
        badge: '只读',
        action: '读取页面结构',
      ),
      (
        name: 'computer_browser_act',
        title: '正在操作隔离网页',
        badge: '会改变状态',
        action: '执行网页动作',
      ),
      (
        name: 'computer_browser_wait',
        title: '正在等待隔离网页',
        badge: '只读',
        action: '等待页面达到指定状态',
      ),
      (name: 'screenshot', title: '正在读取本机屏幕', badge: '只读', action: '截取当前屏幕'),
      (name: 'click', title: '正在操作本机桌面', badge: '会改变状态', action: '点击屏幕'),
      (name: 'type_text', title: '正在操作本机桌面', badge: '会改变状态', action: '输入文字'),
      (name: 'key', title: '正在操作本机桌面', badge: '会改变状态', action: '发送按键'),
      (name: 'scroll', title: '正在操作本机桌面', badge: '会改变状态', action: '滚动屏幕'),
    ];

    for (final item in cases) {
      api.events.add(
        ChatToolEvent(
          name: item.name,
          summary: '调用 ${item.name}',
          phase: ToolPhase.call,
        ),
      );
      await _tick(tester, 3);

      expect(find.text(item.title), findsOneWidget, reason: item.name);
      expect(find.text(item.badge), findsOneWidget, reason: item.name);
      expect(find.text(item.action), findsOneWidget, reason: item.name);

      api.events.add(
        ChatToolEvent(name: item.name, summary: '${item.name} 已完成'),
      );
      await _tick(tester, 3);
      expect(
        find.byKey(const ValueKey('computer_use:status')),
        findsNothing,
        reason: '${item.name} 完成后不能继续显示运行态',
      );
    }

    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);
  });

  testWidgets('参数带定位信息时只展示窗口应用动作，不泄露输入内容', (tester) async {
    final api = _StreamingApi();
    await _startComputerTurn(tester, api);

    api.events.add(
      const ChatToolEvent(
        name: 'computer_act',
        summary:
            '调用 computer_act '
            '(app=Chrome, window_id=win-42, action=click, text=top-secret, keys=ctrl+a)',
        phase: ToolPhase.call,
      ),
    );
    await _tick(tester, 3);

    expect(
      find.text('执行窗口动作 · 应用 Chrome · 窗口 win-42 · 动作 click'),
      findsOneWidget,
      reason: '有真实定位参数时可以显示当前应用、窗口和动作，帮助用户判断正在操作哪里',
    );
    expect(find.textContaining('top-secret'), findsNothing);
    expect(find.textContaining('ctrl+a'), findsNothing);
    expect(find.text('会改变状态'), findsOneWidget);

    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);
  });

  testWidgets('轨迹视图只解析脱敏字段，不展示窗口内容、输入值或凭据', (tester) async {
    const typedText = 'typed-SECRET-001';
    const windowTitle = 'customer-SECRET-002';
    const cookie = 'cookie-SECRET-003';
    const token = 'token-SECRET-004';
    const screenshot = 'raw-png-SECRET-005';
    const preview = 'type_text(text=$typedText, cookie=$cookie)';
    final now = DateTime(2026, 10, 29, 9, 30);
    final message = ChatMessage(
      id: 'assistant-1',
      role: MessageRole.assistant,
      text: '',
      createdAt: now,
      toolCalls: [
        ToolCall(
          name: 'computer_observe',
          result: '返回 1 行 / 880 字符',
          output: jsonEncode({
            'app': '记事本',
            'window_title': windowTitle,
            'total_elements': 3,
            'returned_elements': 2,
            'elements_complete': false,
            'truncated': true,
            'secure_input': true,
            'capture_available': true,
            'capture_id': screenshot,
            'elements': [
              {'label': typedText, 'value': token, 'secure': true},
            ],
            'note': cookie,
          }),
        ),
        ToolCall(
          name: 'computer_act',
          result: '返回 1 行 / 900 字符',
          output: jsonEncode({
            'app': '记事本',
            'action_class': 'Commit',
            'verification_passed': true,
            'verification_evidence': '$typedText $token $cookie',
            'capture_id': screenshot,
          }),
        ),
      ],
    );
    final timestamp = now.add(const Duration(seconds: 2));
    final pending = PendingConfirmation(
      token: token,
      tool: 'computer_act',
      risk: 'execute',
      preview: preview,
      deadline: timestamp.add(const Duration(minutes: 3)),
      sessionId: 'session-1',
    );
    final resolved = ConfirmResolution(
      request: PendingConfirmation(
        token: 'another-token',
        tool: 'computer_observe',
        risk: 'execute',
        preview: preview,
        deadline: timestamp,
        sessionId: 'session-1',
      ),
      outcome: ConfirmOutcome.allowed,
      at: timestamp,
    );

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ComputerUseTraceView(
            messages: [message],
            pendingConfirmations: [pending],
            resolvedConfirmations: [resolved],
          ),
        ),
      ),
    );
    await tester.pump();

    expect(
      find.byKey(const ValueKey('computer_use:trace-boundary')),
      findsOneWidget,
      reason: '轨迹必须先说清只有本次运行可解析到的明细，不能冒充跨重启完整回放',
    );
    expect(find.text('本机内存明细'), findsOneWidget);
    expect(find.textContaining('应用 记事本'), findsAtLeastNWidgets(1));
    expect(find.textContaining('返回 2/3 个元素'), findsOneWidget);
    expect(find.textContaining('安全输入已检测到'), findsOneWidget);
    expect(find.textContaining('截图已捕获（不展示）'), findsOneWidget);
    expect(find.textContaining('提交'), findsWidgets);
    expect(find.textContaining('通过；宿主动作后用新 observation 复核。'), findsOneWidget);
    expect(find.textContaining('等待用户确认'), findsOneWidget);
    expect(find.textContaining('用户已允许'), findsOneWidget);

    for (final secret in [
      typedText,
      windowTitle,
      cookie,
      token,
      screenshot,
      preview,
    ]) {
      expect(
        find.textContaining(secret),
        findsNothing,
        reason: '轨迹视图不能把 $secret 原值带进 widget 树',
      );
    }
  });

  test('普通 trace 的传输取消仍包装为统一 API 错误', () async {
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:9826',
      client: MockClient((request) async {
        throw http.RequestAbortedException(request.url);
      }),
    );
    addTearDown(api.dispose);
    await expectLater(
      api.computerUseTraces('ses_safe_01'),
      throwsA(isA<CortexApiException>()),
    );
    await expectLater(
      api.computerUseTracesAbortable(
        'ses_safe_01',
        abortTrigger: Completer<void>().future,
      ),
      throwsA(isA<http.RequestAbortedException>()),
    );
  });

  test('trace API 保留服务端的审计缺口错误', () async {
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:9826',
      client: MockClient(
        (_) async => http.Response(
          jsonEncode({
            'error': '该会话 Computer Use 历史不完整；请人工核对',
            'retryable': false,
          }),
          409,
          headers: const {'content-type': 'application/json; charset=utf-8'},
        ),
      ),
    );
    addTearDown(api.dispose);
    await expectLater(
      api.computerUseTraces('ses_safe_01'),
      throwsA(
        isA<CortexApiException>()
            .having((e) => e.statusCode, 'statusCode', 409)
            .having((e) => e.message, 'message', contains('历史不完整'))
            .having((e) => e.retryable, '未读取响应体时不猜测 retryable', isNull),
      ),
    );
  });

  test('trace API 只走本机只读端点并解析完整审计字段', () async {
    late http.Request seen;
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:9826',
      token: 'local-inbound',
      client: MockClient((request) async {
        seen = request;
        final payload = _traceHistoryPayload();
        payload['typed_text'] = 'raw-typed-SECRET-api';
        return http.Response(
          jsonEncode(payload),
          200,
          headers: const {'content-type': 'application/json'},
        );
      }),
    );
    addTearDown(api.dispose);

    final history = await api.computerUseTraces('ses_safe_01');

    expect(seen.url.path, '/local/computer-traces/ses_safe_01');
    expect(seen.url.queryParameters, {'include_denials': 'true'});
    expect(seen.headers['authorization'], 'Bearer local-inbound');
    final trace = history.traces.single;
    expect(trace.policyVersion, 'cua-windows-v1');
    expect(trace.events.map((event) => event.type), [
      'observation',
      'policy_decision',
      'confirmation',
      'action',
      'receipt',
      'verification',
      'aborted',
    ]);
    final rendered = [
      for (final event in trace.events)
        for (final field in event.fields) '${field.label}:${field.value}',
    ].join('\n');
    expect(rendered, contains('回执 id:receipt-7'));
    expect(rendered, contains('验证结果:passed'));
    expect(rendered, contains('中止原因:user_cancelled'));
    expect(
      rendered,
      isNot(contains('raw-typed-SECRET-api')),
      reason: '协议层之外的未知字段不能进入受限视图模型',
    );
  });

  test('trace v2 解析派发前拒绝和全部允许的工具名', () {
    for (final tool in [
      'computer_act',
      'computer_browser_act',
      'computer_browser_open',
      'computer_browser_observe',
      'computer_browser_wait',
      'computer_list',
      'computer_observe',
      'computer_wait',
    ]) {
      final trace = ComputerUseTraceHistory.fromJson(
        _denialTraceHistoryPayload(tool: tool),
      ).traces.single;
      final event = trace.events.single;
      expect(trace.outcome, 'aborted');
      expect(event.type, 'pre_dispatch_rejection');
      expect(event.sequence, 1);
      expect(event.title, '用户拒绝（派发前）');
      expect(event.fields.map((field) => field.value), [
        tool,
        '2023-11-14T22:13:20.123Z',
      ]);
      expect(
        trace.fields.map((field) => field.label),
        isNot(contains('应用类型')),
        reason: '拒绝发生在确定应用之前时不能虚构应用身份',
      );
    }

    final withApp = _denialTraceHistoryPayload();
    final trace = (withApp['traces'] as List).single as Map<String, dynamic>;
    trace['app'] = {
      'kind': 'browser_context',
      'stable_id': 'isolated-browser',
      'publisher_verified': false,
    };
    expect(
      ComputerUseTraceHistory.fromJson(withApp).traces.single.fields
          .where((field) => field.label == '稳定应用身份')
          .single
          .value,
      'isolated-browser',
    );
  });

  test('trace API 显式请求后可同时解析 v1 与 v2 历史', () async {
    var requests = 0;
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:9826',
      token: 'local-inbound',
      client: MockClient((request) async {
        requests++;
        expect(request.url.queryParameters['include_denials'], 'true');
        final traces = <Object?>[
          ...(_traceHistoryPayload()['traces'] as List),
          if (request.url.queryParameters['include_denials'] == 'true')
            ...(_denialTraceHistoryPayload()['traces'] as List),
        ];
        return http.Response(
          jsonEncode({'traces': traces}),
          200,
          headers: const {'content-type': 'application/json'},
        );
      }),
    );
    addTearDown(api.dispose);

    final history = await api.computerUseTraces('ses_safe_01');
    expect(requests, 1);
    expect(history.traces, hasLength(2));
    expect(history.traces.first.events.first.type, 'observation');
    expect(history.traces.last.events.single.type, 'pre_dispatch_rejection');
    expect(
      history.traces.last.events.single.fields.first.value,
      'computer_browser_open',
    );
  });

  test('trace API 无损读取完整 u64，并拒绝越界和非整数', () async {
    const maxUint64 = '18446744073709551615';
    var body = '';
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:9826',
      token: 'local-inbound',
      client: MockClient(
        (_) async => http.Response(
          body,
          200,
          headers: const {'content-type': 'application/json'},
        ),
      ),
    );
    addTearDown(api.dispose);

    final payload = _denialTraceHistoryPayload();
    final trace = (payload['traces'] as List).single as Map<String, dynamic>;
    trace['policy_version'] = r'包含数字 123 和转义引号 "sequence": 99';
    final original = jsonEncode(payload);
    body = original
        .replaceFirst('"sequence":1', '"sequence":$maxUint64')
        .replaceFirst(
          '"timestamp_unix_ms":1700000000123',
          '"timestamp_unix_ms":$maxUint64',
        );
    final parsed = (await api.computerUseTraces('ses_safe_01')).traces.single;
    expect(parsed.policyVersion, trace['policy_version']);
    expect(parsed.events.single.sequence, BigInt.parse(maxUint64));
    expect(parsed.events.single.fields.last.value, contains(maxUint64));

    for (final field in ['sequence', 'timestamp_unix_ms']) {
      final originalValue = field == 'sequence' ? '1' : '1700000000123';
      for (final invalid in [
        '18446744073709551616',
        '184467440737095516160000',
        '-1',
        '1.0',
        '1e0',
        '"1"',
        '01',
      ]) {
        body = original.replaceFirst(
          '"$field":$originalValue',
          '"$field":$invalid',
        );
        await expectLater(
          api.computerUseTraces('ses_safe_01'),
          throwsA(isA<CortexApiException>()),
          reason: '$field 不能接受非 u64 JSON 数字 $invalid',
        );
      }
    }
  });

  test('v1 的预览、验证与 URL 旧字段以指纹展示并保留原值', () {
    final payload = _traceHistoryPayload();
    final trace = (payload['traces'] as List).single as Map<String, dynamic>;
    final events = List<Object?>.from(trace['events'] as List);
    trace['events'] = events;
    final verification =
        (events[5] as Map<String, dynamic>)['verification']
            as Map<String, dynamic>;
    verification['check'] = {
      'kind': 'element_state_hash',
      'target_ref': 'element-2',
      'expected_state_hash': 'legacy-state-hash',
    };
    verification['result'] = {
      'status': 'failed',
      'evidence': {
        'code': 'postcondition_not_met',
        'expected_hash': 'legacy-expected-hash',
        'observed_summary_hash': 'legacy-observed-hash',
      },
    };
    events.addAll([
      {
        'sequence': 8,
        'type': 'action',
        'action_id': 8,
        'summary': {'kind': 'navigate', 'url_hash': 'legacy-url-hash'},
      },
      {
        'sequence': 9,
        'type': 'action',
        'action_id': 9,
        'summary': {
          'kind': 'open_application',
          'app_id': 'browser',
          'url_hash': 'legacy-launch-url-hash',
        },
      },
      {
        'sequence': 10,
        'type': 'action',
        'action_id': 10,
        'summary': {'kind': 'wait', 'condition_hash': 'legacy-condition-hash'},
      },
      {
        'sequence': 11,
        'type': 'verification',
        'verification': {
          ...verification,
          'check': {
            'kind': 'window_summary_hash',
            'expected_summary_hash': 'legacy-summary-hash',
          },
        },
      },
    ]);

    final parsed = ComputerUseTraceHistory.fromJson(payload).traces.single;
    final fields = [for (final event in parsed.events) ...event.fields];
    void expectField(String label, String value) {
      expect(
        fields.any((field) => field.label == label && field.value == value),
        isTrue,
        reason: '$label 必须保留 v1 的原始字段值',
      );
    }

    expectField('预览指纹', 'preview-hash');
    expectField('验证条件', '元素状态指纹');
    expectField('验证条件', '窗口摘要指纹');
    expectField('期望状态指纹', 'legacy-state-hash');
    expectField('期望摘要指纹', 'legacy-summary-hash');
    expectField('证据期望指纹', 'legacy-expected-hash');
    expectField('证据观察指纹', 'legacy-observed-hash');
    expectField('动作 URL 指纹', 'legacy-url-hash');
    expectField('动作 URL 指纹', 'legacy-launch-url-hash');
    expectField('动作 条件指纹', 'legacy-condition-hash');
  });

  test('trace v2 拒绝非法工具、版本和时间字段', () {
    for (final payload in [
      _denialTraceHistoryPayload(tool: 'computer_browser_delete'),
      _denialTraceHistoryPayload(tool: 'computer_act\n注入内容'),
      _denialTraceHistoryPayload(sequence: -1),
      _denialTraceHistoryPayload(timestamp: -1),
      _denialTraceHistoryPayload(timestamp: 1.5),
      _denialTraceHistoryPayload(timestamp: '1700000000123'),
    ]) {
      expect(
        () => ComputerUseTraceHistory.fromJson(payload),
        throwsFormatException,
      );
    }

    final v1WithDenial = _denialTraceHistoryPayload();
    final v1Trace =
        (v1WithDenial['traces'] as List).single as Map<String, dynamic>;
    v1Trace['schema_version'] = 1;
    v1Trace['app'] = {
      'kind': 'windows_executable',
      'stable_id': 'publisher/product',
      'publisher_verified': true,
    };
    expect(
      () => ComputerUseTraceHistory.fromJson(v1WithDenial),
      throwsFormatException,
      reason: '新事件不能伪装为旧协议的合法事件',
    );

    final invalidOutcome = _denialTraceHistoryPayload();
    final invalidTrace =
        (invalidOutcome['traces'] as List).single as Map<String, dynamic>;
    invalidTrace['outcome'] = 'completed';
    expect(
      () => ComputerUseTraceHistory.fromJson(invalidOutcome),
      throwsFormatException,
      reason: '用户拒绝的记录不能显示为完成',
    );

    final invalidVersion = _denialTraceHistoryPayload();
    final futureTrace =
        (invalidVersion['traces'] as List).single as Map<String, dynamic>;
    futureTrace['schema_version'] = 3;
    expect(
      () => ComputerUseTraceHistory.fromJson(invalidVersion),
      throwsFormatException,
    );

    for (final version in [1, 2]) {
      final missingApp = version == 1
          ? _traceHistoryPayload()
          : _denialTraceHistoryPayload();
      final missingAppTrace =
          (missingApp['traces'] as List).single as Map<String, dynamic>;
      missingAppTrace.remove('app');
      expect(
        () => ComputerUseTraceHistory.fromJson(missingApp),
        throwsFormatException,
        reason: 'v$version 必须显式包含 app，不能把缺字段当成 null',
      );
    }
  });

  test('超出日期表示范围的拒绝时间保留 Unix 毫秒数', () {
    const timestamp = 8640000000000001;
    final event = ComputerUseTraceHistory.fromJson(
      _denialTraceHistoryPayload(timestamp: timestamp),
    ).traces.single.events.single;
    expect(event.fields.last.value, contains('$timestamp'));
    expect(event.fields.last.value, contains('Unix'));
  });

  testWidgets('仅有历史拒绝时入口仍可见且不虚构执行信息', (tester) async {
    final payload = _denialTraceHistoryPayload();
    final trace = (payload['traces'] as List).single as Map<String, dynamic>;
    final event = (trace['events'] as List).single as Map<String, dynamic>;
    event['params'] = {'secret': 'raw-params-SECRET'};
    event['action'] = {'kind': 'fake-action-SECRET'};
    event['receipt'] = {'receipt_id': 'fake-receipt-SECRET'};
    final api = _TraceApi(ComputerUseTraceHistory.fromJson(payload));
    final c = _boot(api: api);
    addTearDown(c.dispose);
    addTearDown(api.events.close);
    await _pumpTraceButton(tester, c);

    expect(await c.read(chatControllerProvider.notifier).send('查看历史'), isTrue);
    await _tick(tester, 12);
    final sessionId = c.read(chatControllerProvider).activeSessionId;
    expect(api.queriedSessionIds, contains(sessionId));
    expect(
      find.byKey(const ValueKey('computer_use:trace')),
      findsOneWidget,
      reason: '重开会话后即使没有客户端工具事件，也应能查看本机拒绝记录',
    );
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 16);

    expect(find.text('1 · 用户拒绝（派发前）'), findsOneWidget);
    expect(find.text('computer_browser_open'), findsOneWidget);
    expect(find.text('2023-11-14T22:13:20.123Z'), findsOneWidget);
    for (final absent in [
      '应用类型',
      '稳定应用身份',
      '动作 id',
      '回执 id',
      'raw-params-SECRET',
      'fake-action-SECRET',
      'fake-receipt-SECRET',
    ]) {
      expect(find.textContaining(absent), findsNothing);
    }

    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);
  });

  testWidgets('旧后端没有历史端点时探测不会持续重试', (tester) async {
    final api = _MissingTraceApi();
    final c = _boot(api: api);
    addTearDown(c.dispose);
    addTearDown(api.events.close);
    await _pumpTraceButton(tester, c);

    expect(await c.read(chatControllerProvider.notifier).send('普通对话'), isTrue);
    await _tick(tester, 12);
    expect(api.calls, 1);
    await tester.pump(const Duration(seconds: 10));
    expect(api.calls, 1, reason: '404 表示旧后端没有这个端点，重试不会让它出现');
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);

    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);
    expect(api.calls, 2, reason: '轮次结束后允许一次重新探测，但不能持续重试');
  });

  testWidgets('无缓存时 409 后 503 保留入口，随后成功才清除缺口', (tester) async {
    final api = _ScriptedTraceApi();
    await _startTraceProbe(tester, api);
    api.requests[0].result.completeError(
      const CortexApiException('历史不完整', statusCode: 409),
    );
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);

    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    expect(api.requests, hasLength(2));
    api.requests[1].result.completeError(
      const CortexApiException('暂时不可用', statusCode: 503),
    );
    await _tick(tester, 3);
    expect(
      find.byKey(const ValueKey('computer_use:trace')),
      findsOneWidget,
      reason: '503 无法证明之前观察到的审计缺口已经消除',
    );

    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    expect(api.requests, hasLength(3));
    api.requests[2].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
  });

  testWidgets('多次网络错误不消耗后续 409 的有限复查机会', (tester) async {
    final api = _ScriptedTraceApi();
    await _startTraceProbe(tester, api);
    for (final (index, delay) in [1, 3, 8, 20].indexed) {
      api.requests[index].result.completeError(
        const CortexApiException('网络暂时中断'),
      );
      await _tick(tester, 3);
      await tester.pump(Duration(seconds: delay));
      await _tick(tester, 3);
      expect(api.requests, hasLength(index + 2));
    }
    api.requests[4].result.completeError(
      const CortexApiException('历史不完整', statusCode: 409),
    );
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    expect(api.requests, hasLength(6), reason: '409 应从自己的第一次复查开始');
    api.requests[5].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
  });

  testWidgets('旧成功晚于新 409 时不能清除缺口', (tester) async {
    final api = _ScriptedTraceApi();
    final c = await _startTraceProbe(tester, api);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_b');
    await _tick(tester, 3);
    api.requests[1].result.complete(const ComputerUseTraceHistory(traces: []));
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 3);
    expect(api.requests[2].sessionId, 'ses_trace_a');
    api.requests[2].result.completeError(
      const CortexApiException('新缺口', statusCode: 409),
    );
    await _tick(tester, 3);
    api.requests[0].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(
      find.byKey(const ValueKey('computer_use:trace')),
      findsOneWidget,
      reason: '发生在 409 之前的查询不能证明缺口消失',
    );
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    api.requests[3].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
  });

  testWidgets('旧 409 晚于新成功时重新显示缺口并复查', (tester) async {
    final api = _ScriptedTraceApi();
    final c = await _startTraceProbe(tester, api);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_b');
    await _tick(tester, 3);
    api.requests[1].result.complete(const ComputerUseTraceHistory(traces: []));
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 3);
    api.requests[2].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
    api.requests[0].result.completeError(
      const CortexApiException('迟到的缺口', statusCode: 409),
    );
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    expect(api.requests, hasLength(4));
    api.requests[3].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
  });

  testWidgets('显式刷新成功仍需等待并发旧请求，迟到 409 不能被覆盖', (tester) async {
    final api = _ScriptedTraceApi();
    await _startTraceProbe(tester, api);
    api.requests[0].result.completeError(
      const CortexApiException('历史不完整', statusCode: 409),
    );
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 3);
    expect(api.requests, hasLength(2));
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 3);
    expect(api.requests, hasLength(3));

    final history = ComputerUseTraceHistory.fromJson(_traceHistoryPayload());
    api.requests[2].result.complete(history);
    await _tick(tester, 3);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('computer_use:backend-history')),
      findsNothing,
    );
    api.requests[1].result.completeError(
      const CortexApiException('旧查询发现缺口', statusCode: 409),
    );
    await _tick(tester, 3);
    expect(find.textContaining('旧查询发现缺口'), findsOneWidget);
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    expect(api.requests, hasLength(4));
    api.requests[3].result.complete(history);
    await _tick(tester, 3);
    expect(
      find.byKey(const ValueKey('computer_use:backend-history')),
      findsOneWidget,
    );
  });

  testWidgets('旧并发请求结算后重新查询，不缓存缺口期间的历史', (tester) async {
    final api = _ScriptedTraceApi();
    await _startTraceProbe(tester, api);
    api.requests[0].result.completeError(
      const CortexApiException('历史不完整', statusCode: 409),
    );
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 3);
    final history = ComputerUseTraceHistory.fromJson(_traceHistoryPayload());
    api.requests[2].result.complete(history);
    await _tick(tester, 3);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    api.requests[1].result.completeError(const CortexApiException('旧请求断线'));
    await _tick(tester, 3);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    expect(api.requests, hasLength(4), reason: '缺口期间的隐藏历史不能保留');
    api.requests[3].result.complete(history);
    await _tick(tester, 3);
    expect(
      find.byKey(const ValueKey('computer_use:backend-history')),
      findsOneWidget,
      reason: '旧请求结束后的新查询可以证明缺口消除',
    );
    await tester.pump(const Duration(seconds: 30));
    expect(api.requests, hasLength(4), reason: '确认成功后不应继续重试');
  });

  testWidgets('旧 409 在复查预算耗尽时不能取消已安排的网络重试', (tester) async {
    final api = _ScriptedTraceApi();
    await _startTraceProbe(tester, api);
    const gap = CortexApiException('历史不完整', statusCode: 409);
    api.requests[0].result.completeError(gap);
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 3);
    for (final (offset, delay) in [1, 3, 8].indexed) {
      api.requests[offset + 2].result.completeError(gap);
      await _tick(tester, 3);
      await tester.pump(Duration(seconds: delay));
      await _tick(tester, 3);
    }
    api.requests[5].result.completeError(const CortexApiException('网络中断'));
    await _tick(tester, 3);
    api.requests[1].result.completeError(gap);
    await _tick(tester, 3);
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    expect(api.requests, hasLength(7), reason: '旧 409 不能取消已有网络重试');
    api.requests[6].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
  });

  testWidgets('旧请求超时后要重新证明，未取消实现的迟到 409 仍会生效', (tester) async {
    final api = _ScriptedTraceApi();
    await _startTraceProbe(tester, api);
    const gap = CortexApiException('历史不完整', statusCode: 409);
    api.requests[0].result.completeError(gap);
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 3);
    api.requests[2].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    await tester.pump(const Duration(seconds: 15));
    await _tick(tester, 3);
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    expect(api.requests, hasLength(4), reason: '超时后必须发起新查询');
    api.requests[3].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(
      find.textContaining('历史不完整'),
      findsOneWidget,
      reason: '无法确认底层取消时，不能把较新的成功当作完整证明',
    );
    api.requests[1].result.completeError(gap);
    await _tick(tester, 3);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    await tester.pump(const Duration(seconds: 3));
    await _tick(tester, 3);
    api.requests[4].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.textContaining('历史不完整'), findsNothing);
  });

  testWidgets('无已知缺口时旧请求超时也必须重新查询', (tester) async {
    final api = _ScriptedTraceApi();
    final c = await _startTraceProbe(tester, api);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_b');
    await _tick(tester, 3);
    api.requests[1].result.complete(const ComputerUseTraceHistory(traces: []));
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 3);
    api.requests[2].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
    await tester.pump(const Duration(seconds: 15));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    expect(api.requests, hasLength(4));
    api.requests[3].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(
      find.byKey(const ValueKey('computer_use:trace')),
      findsOneWidget,
      reason: '未取消的旧请求尚未结算，必须保留不确定状态',
    );
    api.requests[0].result.completeError(
      const CortexApiException('超时后迟到的缺口', statusCode: 409),
    );
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    api.requests[4].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
  });

  testWidgets('无法取消的旧请求结束后仍须再查询才能解除超时不确定性', (tester) async {
    final api = _ScriptedTraceApi();
    final c = await _startTraceProbe(tester, api);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_b');
    await _tick(tester, 3);
    api.requests[1].result.complete(const ComputerUseTraceHistory(traces: []));
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 3);
    api.requests[2].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    await tester.pump(const Duration(seconds: 15));
    await _tick(tester, 3);
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 3);
    api.requests[3].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    api.requests[0].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(api.requests, hasLength(5), reason: '旧请求结束后需要新证明');
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    api.requests[4].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
  });

  testWidgets('真实 HTTP 错误体的 503 deterministic 不自动重试', (tester) async {
    var calls = 0;
    final httpApi = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:8080',
      client: MockClient((_) async {
        calls++;
        if (calls == 1) {
          return http.Response(
            jsonEncode({'error': '历史不完整', 'retryable': false}),
            409,
            headers: const {'content-type': 'application/json; charset=utf-8'},
          );
        }
        if (calls == 2) {
          return http.Response(
            jsonEncode({'error': '目录暂不可用', 'retryable': false}),
            503,
            headers: const {'content-type': 'application/json; charset=utf-8'},
          );
        }
        return http.Response(jsonEncode({'traces': []}), 200);
      }),
    );
    final api = _HttpTraceApi(httpApi);
    final c = _boot(api: api);
    addTearDown(c.dispose);
    addTearDown(httpApi.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 6);
    expect(calls, 1);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 6);
    expect(calls, 2);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    expect(find.textContaining('目录暂不可用'), findsOneWidget);
    await tester.pump(const Duration(seconds: 60));
    await _tick(tester, 3);
    expect(calls, 2, reason: '真实响应声明不可重试，不能凭状态码猜测');
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 6);
    expect(calls, 3);
    expect(find.textContaining('历史不完整'), findsNothing);
  });

  test('HTTP trace 请求把取消信号交给支持取消的传输', () async {
    final abort = Completer<void>();
    final received = Completer<void>();
    final pending = Completer<http.StreamedResponse>();
    final client = MockClient.streaming((request, _) {
      expect(request, isA<http.AbortableRequest>());
      final trigger = (request as http.Abortable).abortTrigger!;
      received.complete();
      trigger.then((_) {
        pending.completeError(http.RequestAbortedException(request.url));
      });
      return pending.future;
    });
    final api = HttpCortexApi(baseUrl: 'http://127.0.0.1:8080', client: client);
    addTearDown(api.dispose);
    final response = api.computerUseTracesAbortable(
      'ses_trace_a',
      abortTrigger: abort.future,
    );
    await received.future;
    abort.complete();
    await expectLater(response, throwsA(isA<http.RequestAbortedException>()));
  });

  testWidgets('真实 provider 到期触发 HTTP 取消并发起新查询', (tester) async {
    var calls = 0;
    var cancelled = false;
    final pending = Completer<http.StreamedResponse>();
    http.StreamedResponse response(int status, Map<String, Object> body) =>
        http.StreamedResponse(
          Stream.value(utf8.encode(jsonEncode(body))),
          status,
          headers: const {'content-type': 'application/json; charset=utf-8'},
        );
    final client = MockClient.streaming((request, _) async {
      calls++;
      if (calls == 1) {
        return response(409, {'error': '历史不完整', 'retryable': false});
      }
      if (calls == 2) {
        final trigger = (request as http.Abortable).abortTrigger!;
        trigger.then((_) {
          cancelled = true;
          if (!pending.isCompleted) {
            pending.completeError(http.RequestAbortedException(request.url));
          }
        });
        return pending.future;
      }
      return response(200, {'traces': []});
    });
    final httpApi = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:8080',
      client: client,
    );
    final api = _HttpTraceApi(httpApi);
    final c = _boot(api: api);
    addTearDown(c.dispose);
    addTearDown(httpApi.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 6);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 6);
    expect(calls, 2);
    await tester.pump(const Duration(seconds: 15));
    await _tick(tester, 3);
    expect(cancelled, isTrue);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 6);
    expect(calls, 3);
    expect(find.textContaining('历史不完整'), findsNothing);
  });

  testWidgets('最新 404 是终态，迟到 409 保留缺口但不自动复查', (tester) async {
    final api = _ScriptedTraceApi();
    await _startTraceProbe(tester, api);
    const gap = CortexApiException('历史不完整', statusCode: 409);
    api.requests[0].result.completeError(gap);
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 3);
    api.requests[1].result.completeError(gap);
    await _tick(tester, 3);
    api.requests[3].result.completeError(
      const CortexApiException('旧后端没有端点', statusCode: 404),
    );
    await _tick(tester, 3);
    api.requests[2].result.completeError(gap);
    await _tick(tester, 3);
    await tester.pump(const Duration(seconds: 30));
    await _tick(tester, 3);
    expect(api.requests, hasLength(4), reason: '终态必须取消已排定时器并禁止迟到请求重新排定');
    expect(find.textContaining('历史不完整'), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 3);
    api.requests[4].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
  });

  testWidgets('旧后端 404 不重试，也不能清除之前的 409', (tester) async {
    final api = _ScriptedTraceApi();
    await _startTraceProbe(tester, api);
    api.requests[0].result.completeError(
      const CortexApiException('历史不完整', statusCode: 409),
    );
    await _tick(tester, 3);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 3);
    api.requests[1].result.completeError(
      const CortexApiException('旧后端没有端点', statusCode: 404),
    );
    await _tick(tester, 3);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    expect(find.textContaining('旧后端没有端点'), findsOneWidget);
    await tester.pump(const Duration(seconds: 60));
    await _tick(tester, 3);
    expect(api.requests, hasLength(2), reason: '404 不应进入后台重试');
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 3);
    expect(api.requests, hasLength(3));
    api.requests[2].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.textContaining('历史不完整'), findsNothing);
  });

  testWidgets('同一 API 的不同会话各自保留缺口', (tester) async {
    final api = _ScriptedTraceApi();
    final c = await _startTraceProbe(tester, api);
    api.requests[0].result.completeError(
      const CortexApiException('会话 A 缺口', statusCode: 409),
    );
    await _tick(tester, 3);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_b');
    await _tick(tester, 3);
    api.requests[1].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    api.requests[2].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
  });

  testWidgets('切走会话后不保留隐藏的历史结果', (tester) async {
    final api = _ScriptedTraceApi();
    final c = await _startTraceProbe(tester, api);
    api.requests[0].result.complete(
      ComputerUseTraceHistory.fromJson(_traceHistoryPayload()),
    );
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_b');
    await _tick(tester, 3);
    api.requests[1].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 3);
    expect(api.requests, hasLength(3));
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
    api.requests[2].result.complete(const ComputerUseTraceHistory(traces: []));
    await _tick(tester, 3);
  });

  testWidgets('同会话不同 API 的迟到缺口互不污染', (tester) async {
    final first = _ScriptedTraceApi();
    final second = _ScriptedTraceApi();
    var active = first;
    final c = _boot(apiFactory: () => active);
    addTearDown(c.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 3);
    expect(first.requests, hasLength(1));

    active = second;
    c.invalidate(cortexApiProvider);
    await _tick(tester, 3);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 3);
    expect(
      second.requests.map((request) => request.sessionId),
      contains('ses_trace_a'),
    );
    for (final request in second.requests) {
      request.result.complete(const ComputerUseTraceHistory(traces: []));
    }
    first.requests[0].result.completeError(
      const CortexApiException('旧 API 的缺口', statusCode: 409),
    );
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);

    active = first;
    c.invalidate(cortexApiProvider);
    await _tick(tester, 3);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 3);
    expect(
      first.requests.skip(1).map((request) => request.sessionId),
      contains('ses_trace_a'),
    );
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    for (final request in first.requests.skip(1)) {
      request.result.complete(const ComputerUseTraceHistory(traces: []));
    }
    await _tick(tester, 3);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
  });

  testWidgets('同账号续期继承缺口，切换账号后不混入旧账号状态', (tester) async {
    const accountA = Account(userId: 'user-a', username: 'a', schemaName: 'a');
    const accountB = Account(userId: 'user-b', username: 'b', schemaName: 'b');
    var account = accountA;
    http.Response error(int status, String message) => http.Response(
      jsonEncode({'error': message, 'retryable': false}),
      status,
      headers: const {'content-type': 'application/json; charset=utf-8'},
    );
    const localOrigin = 'http://127.0.0.1:51234';
    const deployment = 'http://127.0.0.1:8080';
    final first = _httpTraceTestApi(
      () async => error(409, '账号 A 缺口'),
      baseUrl: localOrigin,
      frontsDeployment: deployment,
    );
    final renewed = _httpTraceTestApi(
      () async => error(503, '历史暂不可用'),
      baseUrl: localOrigin,
      frontsDeployment: deployment,
    );
    final switched = _httpTraceTestApi(
      () async => http.Response('{"traces":[]}', 200),
      baseUrl: localOrigin,
      frontsDeployment: deployment,
    );
    final returned = _httpTraceTestApi(
      () async => error(503, '历史暂不可用'),
      baseUrl: localOrigin,
      frontsDeployment: deployment,
    );
    final authApi = HttpCortexApi(
      baseUrl: deployment,
      client: MockClient((request) async {
        if (request.url.path != '/auth/me') throw StateError('身份请求路径错误');
        return http.Response(
          jsonEncode({
            'user_id': account.userId,
            'username': account.username,
            'schema_name': account.schemaName,
          }),
          200,
          headers: const {'content-type': 'application/json; charset=utf-8'},
        );
      }),
    );
    var active = first;
    final c = _boot(apiFactory: () => active, identityLookup: authApi.whoAmI);
    addTearDown(c.dispose);
    addTearDown(first.dispose);
    addTearDown(renewed.dispose);
    addTearDown(switched.dispose);
    addTearDown(returned.dispose);
    addTearDown(authApi.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);

    active = renewed;
    (c.read(authControllerProvider.notifier) as _TraceIdentityAuth)
        .changeForTest('test-renewed');
    await _tick(tester, 6);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 12);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    expect(find.textContaining('历史暂不可用'), findsOneWidget);
    await tester.tap(find.text('关闭'));
    await _tick(tester, 3);

    account = accountB;
    active = switched;
    (c.read(authControllerProvider.notifier) as _TraceIdentityAuth)
        .changeForTest('test-account-b');
    await _tick(tester, 6);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);

    account = accountA;
    active = returned;
    (c.read(authControllerProvider.notifier) as _TraceIdentityAuth)
        .changeForTest('test-account-a-returned');
    await _tick(tester, 6);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 12);
    expect(find.textContaining('历史不完整'), findsOneWidget);
  });

  testWidgets('账号无法验证时不能把空历史显示为已确认完整', (tester) async {
    var traceCalls = 0;
    final api = _httpTraceTestApi(() async {
      traceCalls++;
      return http.Response('{"traces":[]}', 200);
    });
    final c = _boot(api: api, identityLookup: () async => null);
    addTearDown(c.dispose);
    addTearDown(api.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 12);
    expect(find.textContaining('无法确认当前审计账号与后端'), findsOneWidget);
    expect(find.textContaining('历史查询曾超时'), findsNothing);
    expect(find.text('这个会话还没有 Computer Use 工具事件。'), findsNothing);
    expect(traceCalls, 0, reason: '身份未确认时不应读取或缓存隐藏历史');
  });

  testWidgets('不提供可验证身份的其他 API 也不能读取或声称历史完整', (tester) async {
    final api = _UnprovenTraceApi();
    final c = ProviderContainer(
      overrides: [
        cortexApiProvider.overrideWithValue(api),
        chatControllerProvider.overrideWith(_StaticTraceChat.new),
        confirmControllerProvider.overrideWith(_EmptyTraceConfirm.new),
      ],
    );
    addTearDown(c.dispose);
    await _pumpTraceButton(tester, c);
    await _tick(tester, 6);
    expect(api.calls, 0);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 6);
    expect(find.textContaining('无法确认当前审计账号与后端'), findsOneWidget);
    expect(api.calls, 0);
  });

  testWidgets('身份暂不可用后打开查看器会重新验证', (tester) async {
    const account = Account(userId: 'user-a', username: 'a', schemaName: 'a');
    var available = false;
    final api = _httpTraceTestApi(
      () async => http.Response('{"traces":[]}', 200),
    );
    final c = _boot(
      api: api,
      identityLookup: () async => available ? account : null,
    );
    addTearDown(c.dispose);
    addTearDown(api.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    available = true;
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 12);
    expect(find.textContaining('无法确认当前审计账号与后端'), findsNothing);
    expect(find.text('这个会话还没有 Computer Use 工具事件。'), findsOneWidget);
  });

  testWidgets('身份验证进行中显示等待，失败后才提示人工核对', (tester) async {
    final identity = Completer<Account?>();
    var lookups = 0;
    final api = _httpTraceTestApi(
      () async => http.Response('{"traces":[]}', 200),
    );
    final c = _boot(
      api: api,
      identityLookup: () {
        lookups++;
        return identity.future;
      },
    );
    addTearDown(c.dispose);
    addTearDown(api.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 6);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 6);
    expect(find.text('正在验证审计账号与后端…'), findsOneWidget);
    expect(find.textContaining('无法确认当前审计账号与后端'), findsNothing);
    expect(lookups, 1, reason: '打开窗口不能中断正在进行的身份验证');
    identity.complete(null);
    await _tick(tester, 12);
    expect(find.textContaining('无法确认当前审计账号与后端'), findsOneWidget);
  });

  testWidgets('首次历史查询返回 500 时入口可见并能手动刷新', (tester) async {
    var calls = 0;
    final httpApi = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:8080',
      client: MockClient((_) async {
        calls++;
        if (calls <= 2) {
          return http.Response(
            jsonEncode({'error': '读取历史失败', 'retryable': false}),
            500,
            headers: const {'content-type': 'application/json; charset=utf-8'},
          );
        }
        return http.Response('{"traces":[]}', 200);
      }),
    );
    final c = _boot(api: _HttpTraceApi(httpApi));
    addTearDown(c.dispose);
    addTearDown(httpApi.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 6);
    expect(calls, 1);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 6);
    expect(calls, 2);
    expect(find.textContaining('读取历史失败'), findsOneWidget);
    expect(find.textContaining('无法据此判断操作是否发生'), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 6);
    expect(calls, 3);
    expect(find.textContaining('读取历史失败'), findsNothing);
  });

  testWidgets('同账号切换后端时不能继承原后端缺口', (tester) async {
    const account = Account(userId: 'user-a', username: 'a', schemaName: 'a');
    final first = _httpTraceTestApi(
      () async => http.Response(
        jsonEncode({'error': '原后端缺口', 'retryable': false}),
        409,
        headers: const {'content-type': 'application/json; charset=utf-8'},
      ),
    );
    final second = _httpTraceTestApi(
      () async => http.Response('{"traces":[]}', 200),
      baseUrl: 'http://127.0.0.1:9090',
    );
    var active = first;
    final c = _boot(
      apiFactory: () => active,
      identityLookup: () async => account,
    );
    addTearDown(c.dispose);
    addTearDown(first.dispose);
    addTearDown(second.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    active = second;
    (c.read(appConfigProvider.notifier) as _MockConfig).changeBaseForTest(
      'http://127.0.0.1:9090',
    );
    c.invalidate(cortexApiProvider);
    await _tick(tester, 6);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
  });

  testWidgets('同账号的本机 agent 地址变化时不合并原实例缺口', (tester) async {
    const account = Account(userId: 'user-a', username: 'a', schemaName: 'a');
    const deployment = 'http://127.0.0.1:8080';
    final first = _httpTraceTestApi(
      () async => http.Response(
        jsonEncode({'error': '旧实例缺口', 'retryable': false}),
        409,
        headers: const {'content-type': 'application/json; charset=utf-8'},
      ),
      baseUrl: 'http://127.0.0.1:51234',
      frontsDeployment: deployment,
    );
    final second = _httpTraceTestApi(
      () async => http.Response('{"traces":[]}', 200),
      baseUrl: 'http://127.0.0.1:51235',
      frontsDeployment: deployment,
    );
    var active = first;
    final c = _boot(
      apiFactory: () => active,
      identityLookup: () async => account,
    );
    addTearDown(c.dispose);
    addTearDown(first.dispose);
    addTearDown(second.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    active = second;
    c.invalidate(cortexApiProvider);
    await _tick(tester, 6);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
  });

  test('身份键拒绝带查询参数的后端地址', () {
    final api = HttpCortexApi(baseUrl: 'http://127.0.0.1:8080?extra=1');
    addTearDown(api.dispose);
    expect(api.computerTraceBackendIdentity, isNull);
  });

  test('审计身份拒绝非字符串、空白和控制字符 user_id', () async {
    for (final value in [123, null, '', ' user-a ', 'user\na']) {
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:8080',
        client: MockClient(
          (_) async => http.Response(
            jsonEncode({'user_id': value}),
            200,
            headers: const {'content-type': 'application/json; charset=utf-8'},
          ),
        ),
      );
      expect(
        await api.computerTraceVerifiedUserId(),
        isNull,
        reason: '原始身份字段必须是未经隐式转换的合法字符串',
      );
      api.dispose();
    }
  });

  testWidgets('实际 API 地址与认证部署不一致时不声称身份已确认', (tester) async {
    final api = _httpTraceTestApi(
      () async => http.Response('{"traces":[]}', 200),
      baseUrl: 'http://127.0.0.1:9090',
    );
    var probes = 0;
    final c = _boot(
      api: api,
      identityLookup: () async {
        probes++;
        return const Account(userId: 'user-a', username: 'a', schemaName: 'a');
      },
    );
    addTearDown(c.dispose);
    addTearDown(api.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(probes, 0, reason: '不能用另一个部署的账号探测证明当前后端身份');
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 12);
    expect(find.textContaining('无法确认当前审计账号与后端'), findsOneWidget);
  });

  testWidgets('旧身份探测迟到不能覆盖新账号的审计身份', (tester) async {
    const accountB = Account(userId: 'user-b', username: 'b', schemaName: 'b');
    final oldIdentity = Completer<Account?>();
    var lookupCount = 0;
    final first = _httpTraceTestApi(
      () async => http.Response('{"traces":[]}', 200),
    );
    final second = _httpTraceTestApi(
      () async => http.Response('{"traces":[]}', 200),
    );
    var active = first;
    final c = _boot(
      apiFactory: () => active,
      identityLookup: () {
        lookupCount++;
        return lookupCount == 1 ? oldIdentity.future : Future.value(accountB);
      },
    );
    addTearDown(c.dispose);
    addTearDown(first.dispose);
    addTearDown(second.dispose);
    await _pumpTraceButton(tester, c);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 3);
    active = second;
    (c.read(authControllerProvider.notifier) as _TraceIdentityAuth)
        .changeForTest('test-account-b');
    await _tick(tester, 6);
    c.read(chatControllerProvider.notifier).selectSession('ses_trace_a');
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
    oldIdentity.complete(
      const Account(userId: 'user-a', username: 'a', schemaName: 'a'),
    );
    await _tick(tester, 6);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);
  });

  testWidgets('审计历史不完整时没有本地事件也显示入口与错误', (tester) async {
    final api = _IncompleteTraceApi();
    final c = _boot(api: api);
    addTearDown(c.dispose);
    addTearDown(api.events.close);
    await _pumpTraceButton(tester, c);

    expect(await c.read(chatControllerProvider.notifier).send('普通对话'), isTrue);
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 12);
    expect(find.textContaining('该会话 Computer Use 历史不完整'), findsOneWidget);
    expect(find.textContaining('请人工核对'), findsOneWidget);
    expect(find.textContaining('无法据此判断操作是否发生'), findsOneWidget);
    expect(find.text('这个会话还没有 Computer Use 工具事件。'), findsNothing);
    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);
    for (final seconds in [1, 3, 8]) {
      await tester.pump(Duration(seconds: seconds));
      await _tick(tester, 3);
    }
    final calls = api.calls;
    await tester.pump(const Duration(seconds: 30));
    expect(api.calls, calls, reason: '有限复查结束后不能无限读取审计历史');
  });

  testWidgets('刷新报错时不将缓存的历史展示为完整审计', (tester) async {
    final history = ComputerUseTraceHistory.fromJson(_traceHistoryPayload());
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ComputerUseTraceView(
            messages: const [],
            history: history,
            historyError: 'HTTP 409 · 历史不完整',
          ),
        ),
      ),
    );
    expect(find.textContaining('历史不完整'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('computer_use:backend-history')),
      findsNothing,
    );
  });

  testWidgets('轮次结束后重新查询，空历史变缺口时显示入口', (tester) async {
    final api = _ChangingTraceApi(const ComputerUseTraceHistory(traces: []));
    final c = _boot(api: api);
    addTearDown(c.dispose);
    addTearDown(api.events.close);
    await _pumpTraceButton(tester, c);
    expect(await c.read(chatControllerProvider.notifier).send('普通对话'), isTrue);
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsNothing);

    api.failure = const CortexApiException('历史不完整', statusCode: 409);
    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 12);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    api.failure = null;
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 5);
  });

  testWidgets('打开时重新查询并通过 provider 显示缺口，刷新可恢复', (tester) async {
    final api = _ChangingTraceApi(
      ComputerUseTraceHistory.fromJson(_traceHistoryPayload()),
    );
    final c = _boot(api: api);
    addTearDown(c.dispose);
    addTearDown(api.events.close);
    await _pumpTraceButton(tester, c);
    expect(await c.read(chatControllerProvider.notifier).send('查看历史'), isTrue);
    await _tick(tester, 12);

    final pending = Completer<ComputerUseTraceHistory>();
    api.pending = pending.future;
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 5);
    expect(
      find.byKey(const ValueKey('computer_use:backend-history')),
      findsNothing,
    );
    pending.completeError(const CortexApiException('历史不完整', statusCode: 409));
    await _tick(tester, 12);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('computer_use:backend-history')),
      findsNothing,
    );

    api.pending = null;
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 12);
    expect(
      find.byKey(const ValueKey('computer_use:backend-history')),
      findsOneWidget,
    );
    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);
  });

  testWidgets('后台审计写入完成后有限复查可自动恢复', (tester) async {
    final api = _ChangingTraceApi(
      ComputerUseTraceHistory.fromJson(_traceHistoryPayload()),
    );
    api.failure = const CortexApiException('历史不完整', statusCode: 409);
    final c = _boot(api: api);
    addTearDown(c.dispose);
    addTearDown(api.events.close);
    await _pumpTraceButton(tester, c);
    expect(await c.read(chatControllerProvider.notifier).send('查看历史'), isTrue);
    await _tick(tester, 12);
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 12);
    expect(find.textContaining('历史不完整'), findsOneWidget);

    api.failure = null;
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 12);
    expect(
      find.byKey(const ValueKey('computer_use:backend-history')),
      findsOneWidget,
    );
    api.failure = const CortexApiException('历史不完整', statusCode: 409);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 12);
    expect(find.textContaining('历史不完整'), findsOneWidget);
    api.failure = null;
    await tester.pump(const Duration(seconds: 1));
    await _tick(tester, 12);
    expect(
      find.byKey(const ValueKey('computer_use:backend-history')),
      findsOneWidget,
      reason: '新一轮查询应重新获得有限复查机会',
    );
    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);
  });

  testWidgets('历史视图优先使用后端 trace，并可刷新完整审计字段', (tester) async {
    final api = _TraceApi(
      ComputerUseTraceHistory.fromJson(_traceHistoryPayload()),
    );
    final c = _boot(api: api);
    addTearDown(c.dispose);
    addTearDown(api.events.close);
    await _pumpTraceButton(tester, c);

    expect(await c.read(chatControllerProvider.notifier).send('看一下窗口'), isTrue);
    await _tick(tester, 3);
    api.events.add(
      const ChatToolEvent(
        name: 'computer_observe',
        summary: '调用 computer_observe',
        phase: ToolPhase.call,
      ),
    );
    await _tick(tester, 3);
    final sessionId = c.read(chatControllerProvider).activeSessionId;
    expect(sessionId, isNotNull);

    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 16);

    expect(
      find.byKey(const ValueKey('computer_use:backend-history')),
      findsOneWidget,
      reason: '后端有脱敏历史时应优先于旧工具摘要',
    );
    expect(api.queriedSessionIds, contains(sessionId));
    expect(find.text('历史摘要'), findsNothing);
    expect(find.text('cua-windows-v1'), findsOneWidget);
    expect(find.textContaining('策略判定'), findsOneWidget);
    expect(find.text('allow'), findsOneWidget);
    expect(find.text('预览指纹'), findsOneWidget);
    expect(find.text('preview-hash'), findsOneWidget);
    expect(find.textContaining('不能跨进程比较'), findsOneWidget);
    expect(find.textContaining('驱动回执'), findsOneWidget);
    expect(find.text('receipt-7'), findsOneWidget);
    expect(find.textContaining('动作后验证'), findsOneWidget);
    expect(find.text('passed'), findsOneWidget);
    expect(find.text('7 · 中止'), findsOneWidget);
    expect(find.text('user_cancelled'), findsOneWidget);
    expect(find.text('已脱敏'), findsWidgets);

    final callsBeforeRefresh = api.calls;
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    await _tick(tester, 12);
    expect(
      api.calls,
      greaterThan(callsBeforeRefresh),
      reason: '刷新按钮必须让本机历史重新查询，而不是只重画旧 future',
    );

    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);
  });

  testWidgets('刷新后缺失的输出会显示为历史摘要和明确缺口', (tester) async {
    final message = ChatMessage(
      id: 'assistant-history',
      role: MessageRole.assistant,
      text: '',
      createdAt: DateTime(2026, 10, 29, 9, 30),
      toolCalls: const [
        ToolCall(name: 'computer_act', result: '返回 1 行 / 42 字符'),
        ToolCall(name: 'computer_wait'),
      ],
    );

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(body: ComputerUseTraceView(messages: [message])),
      ),
    );
    await tester.pump();

    expect(
      find.text('历史摘要'),
      findsOneWidget,
      reason: 'output 不随历史 tool-call 持久化，UI 不能把它画成完整明细',
    );
    expect(find.textContaining('观察输出、截图和元素明细未持久化'), findsOneWidget);
    expect(find.textContaining('验证明细未持久化'), findsAtLeastNWidgets(1));
    expect(
      find.textContaining('本机历史 trace 尚不可用或为空'),
      findsOneWidget,
      reason: '没有后端历史时必须明确说明正在回退旧摘要',
    );
    expect(
      find.textContaining('旧摘要不含策略版本、驱动回执'),
      findsOneWidget,
      reason: '回退视图不能把旧工具事件画成完整审计闭环',
    );
    expect(
      find.textContaining('客户端的这一轮已结束，但没有收到工具结果'),
      findsOneWidget,
      reason: '停止生成或断流时不能把没有回执的调用永远画成仍在等待',
    );
  });

  testWidgets('出现电脑工具后可以从聊天顶栏打开只读轨迹', (tester) async {
    await tester.binding.setSurfaceSize(const Size(390, 700));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    final api = _StreamingApi();
    final c = _boot(api: api);
    addTearDown(c.dispose);
    addTearDown(api.events.close);
    await _pumpTraceButton(tester, c);

    expect(await c.read(chatControllerProvider.notifier).send('看一下窗口'), isTrue);
    await _tick(tester, 3);
    api.events.add(
      const ChatToolEvent(
        name: 'computer_observe',
        summary: '调用 computer_observe',
        phase: ToolPhase.call,
      ),
    );
    await _tick(tester, 3);

    expect(
      find.byKey(const ValueKey('computer_use:trace')),
      findsOneWidget,
      reason: '当前轮出现电脑工具时，用户要能从聊天页直接查看发生了什么',
    );
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    await _tick(tester, 12);

    expect(find.text('Computer Use 轨迹'), findsOneWidget);
    expect(find.text('只读'), findsWidgets);
    expect(
      find.textContaining('本机历史 trace 尚不可用'),
      findsOneWidget,
      reason: '测试替身没有历史端点时必须明确回退到旧摘要',
    );

    api.events.add(const ChatDoneEvent(null));
    await _tick(tester, 3);
  });

  test('allowlist 会拒绝相对路径、非 exe 与重复项', () async {
    final expected = <String>[
      r'C:\Program Files\Google\Chrome\Application\chrome.exe',
      r'C:\Windows\System32\notepad.exe',
    ];
    final c = _boot(
      settings: {
        'computer_use_allowlist': jsonEncode([
          ...expected,
          ...expected,
          'chrome.exe',
          r'C:\tmp\readme.txt',
        ]),
      },
    );
    addTearDown(c.dispose);

    expect(
      await _settledAllowlist(c),
      expected,
      reason: 'allowlist 是执行边界，不能接受 basename、非 exe 或大小写重复的身份',
    );
  });

  test('浏览站点 allowlist 只保留公网域名并保持空表即关闭', () async {
    final c = _boot(
      settings: {
        'computer_browser_allowlist': jsonEncode([
          'example.com',
          '*.Docs.Example.com:443',
          'example.com',
          'localhost',
          '127.0.0.1',
          'https://example.com',
        ]),
      },
    );
    addTearDown(c.dispose);

    expect(await _settledBrowserAllowlist(c), [
      'example.com',
      '*.docs.example.com:443',
    ]);

    final empty = _boot();
    addTearDown(empty.dispose);
    expect(await _settledBrowserAllowlist(empty), isEmpty);
  });

  test('打包应用 allowlist 要求身份与精确 exe 同时成立', () async {
    const aumid =
        r'aumid|Microsoft.WindowsCalculator_8wekyb3d8bbwe!App|'
        r'C:\Program Files\WindowsApps\Calculator\Calculator.exe';
    const family =
        r'package-family|Microsoft.WindowsCalculator_8wekyb3d8bbwe|'
        r'C:\Program Files\WindowsApps\Calculator\Calculator.exe';
    final c = _boot(
      settings: {
        'computer_packaged_app_allowlist': jsonEncode([
          aumid,
          family,
          'aumid|missing-bang|'
              r'C:\Program Files\WindowsApps\Calculator\Calculator.exe',
          'package-family|Microsoft.WindowsCalculator_8wekyb3d8bbwe|'
              r'relative.exe',
        ]),
      },
    );
    addTearDown(c.dispose);

    expect(await _settledPackagedAllowlist(c), [aumid, family]);
  });

  testWidgets('设置页展示并允许移除 allowlist 中的精确 exe', (tester) async {
    final first = r'C:\Program Files\Google\Chrome\Application\chrome.exe';
    final second = r'C:\Windows\System32\notepad.exe';
    final written = <String, Map<String, String>>{};
    final c = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        settingsReaderProvider.overrideWithValue(
          () async => {
            'computer_use_allowlist': jsonEncode([first, second]),
          },
        ),
        settingsWriterProvider.overrideWithValue((patch) async {
          written['last'] = patch;
        }),
        healthProvider.overrideWith(
          (_) async => const HealthStatus(
            status: 'ok',
            version: '0.1',
            database: 'unknown',
            role: HealthStatus.roleLocalAgent,
            computerUse: true,
            computerCapabilities: ComputerCapabilities(
              listApps: true,
              listWindows: true,
              observe: true,
              act: true,
              browser: false,
              backgroundInput: true,
              secureInputDetection: true,
            ),
          ),
        ),
        selectedModelVisionProvider.overrideWithValue(true),
      ],
    );
    addTearDown(c.dispose);
    c.read(computerUseAllowlistProvider);

    await _pump(tester, c);
    await _tick(tester);
    expect(c.read(computerUseAllowlistProvider), [first, second]);
    expect(find.text('chrome.exe'), findsOneWidget);
    expect(find.text('notepad.exe'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('computer_use:allowlist-add')),
      findsOneWidget,
    );

    await tester.tap(
      find.descendant(
        of: find.byKey(ValueKey('computer_use:allowlist:$first')),
        matching: find.byIcon(Icons.close_rounded),
      ),
    );
    await _tick(tester);

    expect(c.read(computerUseAllowlistProvider), [second]);
    expect(jsonDecode(written['last']!['computer_use_allowlist']!), [
      second,
    ], reason: '界面显示的新边界必须持久化，否则重启后会恢复成旧权限');
  });

  testWidgets('设置页可添加隔离浏览站点，并明确空表关闭能力', (tester) async {
    final written = <String, Map<String, String>>{};
    final c = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        settingsReaderProvider.overrideWithValue(
          () async => const <String, String>{},
        ),
        settingsWriterProvider.overrideWithValue((patch) async {
          written['last'] = patch;
        }),
        healthProvider.overrideWith(
          (_) async => const HealthStatus(
            status: 'ok',
            version: '0.1',
            database: 'unknown',
            role: HealthStatus.roleLocalAgent,
            computerUse: true,
            computerCapabilities: ComputerCapabilities(
              listApps: true,
              listWindows: true,
              observe: true,
              act: true,
              browser: true,
              backgroundInput: true,
              secureInputDetection: true,
            ),
          ),
        ),
        selectedModelVisionProvider.overrideWithValue(true),
      ],
    );
    addTearDown(c.dispose);
    c.read(computerBrowserAllowlistProvider);

    await _pump(tester, c);
    await _tick(tester);
    expect(find.textContaining('列表为空时网页工具关闭'), findsOneWidget);

    await tester.ensureVisible(
      find.byKey(const ValueKey('computer_use:site-input')),
    );
    await tester.enterText(
      find.byKey(const ValueKey('computer_use:site-input')),
      '*.example.com:443',
    );
    await tester.tap(find.byKey(const ValueKey('computer_use:site-add')));
    await _tick(tester);

    expect(c.read(computerBrowserAllowlistProvider), ['*.example.com:443']);
    expect(jsonDecode(written['last']!['computer_browser_allowlist']!), [
      '*.example.com:443',
    ]);
  });

  testWidgets('设置页可以绑定 Windows 打包应用身份', (tester) async {
    final written = <String, Map<String, String>>{};
    final c = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        settingsReaderProvider.overrideWithValue(
          () async => const <String, String>{},
        ),
        settingsWriterProvider.overrideWithValue((patch) async {
          written['last'] = patch;
        }),
        healthProvider.overrideWith(
          (_) async => const HealthStatus(
            status: 'ok',
            version: '0.1',
            database: 'unknown',
            role: HealthStatus.roleLocalAgent,
            computerUse: true,
          ),
        ),
      ],
    );
    addTearDown(c.dispose);
    c.read(computerPackagedAllowlistProvider);

    await _pump(tester, c);
    await _tick(tester);
    final title = find.text('Windows 打包应用');
    await tester.ensureVisible(title);
    await tester.tap(title);
    await _tick(tester);

    const identity = 'Microsoft.WindowsCalculator_8wekyb3d8bbwe!App';
    const executable =
        r'C:\Program Files\WindowsApps\Calculator\Calculator.exe';
    await tester.enterText(
      find.byKey(const ValueKey('computer_use:packaged-identity')),
      identity,
    );
    await tester.enterText(
      find.byKey(const ValueKey('computer_use:packaged-exe')),
      executable,
    );
    await tester.tap(find.byKey(const ValueKey('computer_use:packaged-add')));
    await _tick(tester);

    expect(c.read(computerPackagedAllowlistProvider), [
      'aumid|$identity|$executable',
    ]);
    expect(jsonDecode(written['last']!['computer_packaged_app_allowlist']!), [
      'aumid|$identity|$executable',
    ]);
  });

  testWidgets('拨开关会存下来', (tester) async {
    final written = <String, String>{};
    final c = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        settingsReaderProvider.overrideWithValue(
          () async => const <String, String>{},
        ),
        settingsWriterProvider.overrideWithValue((patch) async {
          written.addAll(patch);
        }),
        healthProvider.overrideWith(
          (_) async => const HealthStatus(
            status: 'ok',
            version: '0.1',
            database: 'unknown',
            role: HealthStatus.roleLocalAgent,
            computerUse: true,
          ),
        ),
      ],
    );
    addTearDown(c.dispose);

    await _pump(tester, c);
    await _tick(tester);
    await tester.tap(find.byKey(const ValueKey('computer_use:toggle')));
    await _tick(tester);

    expect(
      written['computer_use'],
      'true',
      reason: '不存下来的话，重启之后它悄悄关了 —— 而用户以为自己开着',
    );
  });
}
