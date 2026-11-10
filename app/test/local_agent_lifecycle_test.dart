@TestOn('vm')
library;

import 'dart:async';

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/local_agent.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _LiveConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:9');
}

class _MutableAuth extends AuthController {
  _MutableAuth(this.initial);

  final AuthState initial;

  @override
  AuthState build() => initial;

  void go(AuthState next) => state = next;
}

class _EmptyComputerUseAllowlist extends ComputerUseAllowlistNotifier {
  @override
  List<String> build() => const [];
}

class _EmptyPackagedAllowlist extends ComputerPackagedAllowlistNotifier {
  @override
  List<String> build() => const [];
}

class _EmptyBrowserAllowlist extends ComputerBrowserAllowlistNotifier {
  @override
  List<String> build() => const [];
}

class _FakeAgent extends LocalAgent {
  _FakeAgent({
    required this.id,
    required this.originValue,
    this.stopGate,
    this.events,
  }) : super(executable: 'unused-$id', stateDir: 'unused-$id');

  final String id;
  final String originValue;
  final Completer<void>? stopGate;
  final List<String>? events;

  int startCalls = 0;
  int stopCalls = 0;
  final List<String> inboundTokens = [];
  final List<String?> outboundTokens = [];
  String? _origin;

  @override
  String? get origin => _origin;

  @override
  Future<String> start({
    required String remote,
    required String token,
    String? userToken,
    String? llmRoute,
    bool allowRemoteAttach = false,
    List<String> computerUseAllowlist = const [],
    List<String> packagedComputerAllowlist = const [],
    List<String> browserSiteAllowlist = const [],
    Map<String, String>? extraEnv,
    Duration timeout = const Duration(seconds: 20),
    void Function(int code, String logTail)? onExit,
  }) async {
    startCalls++;
    inboundTokens.add(token);
    outboundTokens.add(userToken);
    events?.add('$id.start');
    _origin = originValue;
    return originValue;
  }

  @override
  Future<void> stop() async {
    stopCalls++;
    events?.add('$id.stop.start');
    final gate = stopGate;
    if (gate != null) await gate.future;
    _origin = null;
    events?.add('$id.stop.end');
  }
}

typedef _PushCall = ({String origin, String inbound, String? outbound});

const _tokenHealth = HealthStatus(
  status: 'ok',
  version: 'test',
  database: 'ok',
  auth: 'token',
);

const _disabledHealth = HealthStatus(
  status: 'ok',
  version: 'test',
  database: 'ok',
  auth: 'disabled',
);

AuthState _ready(String? token, {HealthStatus health = _tokenHealth}) =>
    AuthState(phase: AuthPhase.ready, token: token, health: health);

ProviderContainer _boot({
  required _MutableAuth auth,
  required List<LocalAgent> discovered,
  required LocalAgentCredentialPush push,
}) {
  var discoveryCalls = 0;
  final container = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_LiveConfig.new),
      authControllerProvider.overrideWith(() => auth),
      localAgentDiscoveryProvider.overrideWithValue(() {
        final index = discoveryCalls < discovered.length
            ? discoveryCalls
            : discovered.length - 1;
        discoveryCalls++;
        return discovered[index];
      }),
      localAgentCredentialPushProvider.overrideWithValue(push),
      settingsReaderProvider.overrideWithValue(
        () async => const <String, String>{},
      ),
      computerUseAllowlistProvider.overrideWith(_EmptyComputerUseAllowlist.new),
      computerPackagedAllowlistProvider.overrideWith(
        _EmptyPackagedAllowlist.new,
      ),
      computerBrowserAllowlistProvider.overrideWith(_EmptyBrowserAllowlist.new),
    ],
  );
  addTearDown(container.dispose);
  container.listen(localAgentOriginProvider, (_, _) {}, fireImmediately: true);
  return container;
}

Future<void> _until(
  bool Function() condition, {
  required String reason,
  Duration timeout = const Duration(seconds: 3),
}) async {
  final deadline = DateTime.now().add(timeout);
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) fail('等待超时：$reason');
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

int? _apiPort(ProviderContainer container) {
  final api = container.read(cortexApiProvider);
  if (api is! HttpCortexApi) return null;
  return api.computerTraceBackendIdentity?.endpoint.port;
}

void main() {
  test('凭据推送的状态必须分成接受、换账号与失败三档', () {
    expect(
      agentCredentialPushResultForStatus(204),
      LocalAgentCredentialPushResult.accepted,
      reason: '204 是服务端验明 deployment/user 未变后允许热更新的唯一成功信号',
    );
    expect(
      agentCredentialPushResultForStatus(409),
      LocalAgentCredentialPushResult.identityChanged,
      reason: '409 必须进入换账号流程，后续先停旧进程再起新进程',
    );
    for (final status in [401, 409 + 1, 500]) {
      expect(
        agentCredentialPushResultForStatus(status),
        LocalAgentCredentialPushResult.failed,
        reason: 'HTTP $status 无法证明仍属同一账号，必须失败关闭',
      );
    }
  });

  test('同账号 token 轮换只热推；确认前走远端，确认后回原进程', () async {
    final refreshPush = Completer<LocalAgentCredentialPushResult>();
    final calls = <_PushCall>[];
    final agent = _FakeAgent(id: 'A', originValue: 'http://127.0.0.1:41001');
    final auth = _MutableAuth(_ready('token-a'));
    final container = _boot(
      auth: auth,
      discovered: [agent],
      push:
          ({
            required String origin,
            required String inbound,
            required String? outbound,
          }) {
            calls.add((origin: origin, inbound: inbound, outbound: outbound));
            return outbound == 'token-a2'
                ? refreshPush.future
                : Future.value(LocalAgentCredentialPushResult.accepted);
          },
    );
    addTearDown(() {
      if (!refreshPush.isCompleted) {
        refreshPush.complete(LocalAgentCredentialPushResult.failed);
      }
    });

    await container.read(localAgentOriginProvider.future);
    expect(_apiPort(container), 41001, reason: '初始凭据 204 后应进入本机 agent');
    expect(agent.startCalls, 1);
    expect(calls.single.inbound, agent.inboundTokens.single);
    expect(calls.single.inbound, isNot('token-a'), reason: '本机控制不能复用账号凭据');
    expect(agent.outboundTokens, ['token-a']);
    expect(calls.single.outbound, 'token-a');

    auth.go(_ready('token-a2'));
    await _until(() => calls.length == 2, reason: 'token 轮换没有进入热推队列');

    expect(
      _apiPort(container),
      9,
      reason: '新 token 尚未被 agent 接住时，主 API 必须立刻走远端',
    );
    expect(agent.stopCalls, 0, reason: '同一账号续期不应关闭仍在运行的 agent');
    expect(agent.startCalls, 1, reason: '同一账号续期不应启动第二个 agent');
    expect(
      calls.last.inbound,
      agent.inboundTokens.single,
      reason: '同账号续期仍钉住当前进程的控制 key',
    );
    expect(calls.last.outbound, 'token-a2');

    refreshPush.complete(LocalAgentCredentialPushResult.accepted);
    await _until(
      () => _apiPort(container) == 41001,
      reason: '204 后没有恢复已验明的本机路由',
    );
    expect(agent.stopCalls, 0);
    expect(agent.startCalls, 1);
  });

  for (final failure in [
    LocalAgentCredentialPushResult.identityChanged,
    LocalAgentCredentialPushResult.failed,
  ]) {
    test('热推 ${failure.name}：先撤旧路由并等旧进程退出，再启动新进程', () async {
      final stopGate = Completer<void>();
      final events = <String>[];
      final first = _FakeAgent(
        id: 'A',
        originValue: 'http://127.0.0.1:41001',
        stopGate: stopGate,
        events: events,
      );
      final second = _FakeAgent(
        id: 'B',
        originValue: 'http://127.0.0.1:41002',
        events: events,
      );
      final calls = <_PushCall>[];
      final auth = _MutableAuth(_ready('token-a'));
      final container = _boot(
        auth: auth,
        discovered: [first, second],
        push:
            ({
              required String origin,
              required String inbound,
              required String? outbound,
            }) {
              calls.add((origin: origin, inbound: inbound, outbound: outbound));
              if (origin == first.originValue && outbound == 'token-b') {
                return Future.value(failure);
              }
              return Future.value(LocalAgentCredentialPushResult.accepted);
            },
      );
      addTearDown(() {
        if (!stopGate.isCompleted) stopGate.complete();
      });

      await container.read(localAgentOriginProvider.future);
      expect(_apiPort(container), 41001);

      // 不经过 logout/needsToken：两个账号都以 ready 状态出现，测试真正盯的是
      // credential push 对 deployment/user 的裁决，而不是 token 是否为 null。
      auth.go(_ready('token-b'));
      await _until(() => first.stopCalls == 1, reason: '失败推送没有触发旧 agent 关闭');

      expect(_apiPort(container), 9, reason: '旧 agent 尚未停稳时，新 token 不能继续路由到它');
      expect(second.startCalls, 0, reason: '旧进程未退出前不能拉起新账号的 agent');

      stopGate.complete();
      await _until(
        () => second.startCalls == 1,
        reason: '旧进程退出后没有按新 token 重建 agent',
      );
      await _until(
        () => _apiPort(container) == 41002,
        reason: '新 agent 的启动凭据未被接受为 ready',
      );

      final stopFinished = events.indexOf('A.stop.end');
      final nextStarted = events.indexOf('B.start');
      expect(
        stopFinished,
        greaterThanOrEqualTo(0),
        reason: '旧进程必须走完整的 close 完成路径',
      );
      expect(
        nextStarted,
        greaterThan(stopFinished),
        reason: '生命周期必须严格是 stop(A) 完成后才 start(B)',
      );
      expect(second.inboundTokens, hasLength(1));
      expect(
        second.inboundTokens.single,
        isNot(first.inboundTokens.single),
        reason: '新账号实例必须重签控制 key',
      );
      expect(second.outboundTokens, ['token-b']);
      expect(calls.last.inbound, second.inboundTokens.single);
      expect(calls.last.outbound, 'token-b');
    });
  }

  test('初始凭据推送被 409 拒绝时不得宣布本地 agent ready', () async {
    final agent = _FakeAgent(id: 'A', originValue: 'http://127.0.0.1:41001');
    final auth = _MutableAuth(_ready('token-a'));
    final container = _boot(
      auth: auth,
      discovered: [agent],
      push:
          ({
            required String origin,
            required String inbound,
            required String? outbound,
          }) async => LocalAgentCredentialPushResult.identityChanged,
    );

    expect(await container.read(localAgentOriginProvider.future), isNull);
    expect(_apiPort(container), 9);
    expect(agent.stopCalls, 1, reason: '身份未通过验证的进程必须立即关闭');
  });

  test('关认证部署在 ready 且无用户 token 时仍会启动本地 agent', () async {
    final calls = <_PushCall>[];
    final agent = _FakeAgent(
      id: 'no-auth',
      originValue: 'http://127.0.0.1:41003',
    );
    final auth = _MutableAuth(_ready(null, health: _disabledHealth));
    final container = _boot(
      auth: auth,
      discovered: [agent],
      push:
          ({
            required String origin,
            required String inbound,
            required String? outbound,
          }) {
            calls.add((origin: origin, inbound: inbound, outbound: outbound));
            return Future.value(LocalAgentCredentialPushResult.accepted);
          },
    );

    expect(
      await container.read(localAgentOriginProvider.future),
      'http://127.0.0.1:41003',
    );
    expect(agent.startCalls, 1);
    expect(calls.single.outbound, isNull);
    expect(calls.single.inbound, isNotEmpty);
    expect(_apiPort(container), 41003);
  });
}
