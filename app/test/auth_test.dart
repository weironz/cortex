import 'package:cortex_app/models/skill.dart';
import 'package:cortex_app/models/assistant.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'dart:async';
import 'package:cortex_app/models/auth_tokens.dart';
import 'package:cortex_app/models/import_plan.dart';
import 'package:cortex_app/import/import_source.dart';
import 'dart:convert';
import 'dart:typed_data';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/models/workspace.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
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
import 'package:cortex_app/state/auth_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

/// Records every request the client makes, so the *shape* of the credential on
/// the wire can be asserted — which header carried it, and which URL did not.
class _Recorder {
  final List<http.BaseRequest> requests = [];

  MockClient client(
    Future<http.Response> Function(http.Request request) handler,
  ) => MockClient((request) {
    requests.add(request);
    return handler(request);
  });

  http.BaseRequest requestTo(String path) =>
      requests.firstWhere((r) => r.url.path == path);
}

http.Response _json(Object body, {int status = 200}) => http.Response(
  jsonEncode(body),
  status,
  headers: {'content-type': 'application/json; charset=utf-8'},
);

const _health = {
  'status': 'ok',
  'version': '0.0.1',
  'database': 'ok',
  'blob_backend': 's3',
  'auth': 'token',
};

void main() {
  // flutter_secure_storage 走平台通道，而通道要 binding。没有这一句，
  // 存凭据会抛「Binding has not yet been initialized」—— 那条异常被
  // rememberToken 吞掉并打成一行日志（有意的），于是测试**看起来只是
  // 断言不过**，真因藏在输出里
  TestWidgetsFlutterBinding.ensureInitialized();
  _sessionTests();
  _resilienceTests();
  _registerTests();
  group('/health 的 auth 字段', () {
    test('token 表示必须带凭据', () {
      final health = HealthStatus.fromJson(_health);
      expect(health.auth, 'token');
      expect(health.requiresToken, isTrue);
      expect(health.authDisabled, isFalse);
    });

    test('disabled 表示服务端不检查，客户端就不该逼用户填 token', () {
      final health = HealthStatus.fromJson({..._health, 'auth': 'disabled'});
      expect(health.requiresToken, isFalse);
      expect(
        health.authDisabled,
        isTrue,
        reason: '这一条要单独可查：关掉认证是个必须被说出来的部署状态，不能只是「不用登录」',
      );
    });

    test('老 daemon 没有这个字段时按「需要凭据」处理', () {
      final health = HealthStatus.fromJson({
        'status': 'ok',
        'version': '0.0.1',
        'database': 'ok',
      });
      expect(health.auth, HealthStatus.authUnknown);
      expect(
        health.requiresToken,
        isTrue,
        reason:
            '猜 disabled 会把用户送进一个每次调用都 401、且没有任何入口回到 token '
            '输入框的界面；猜 token 最坏只是多问一次',
      );
    });
  });

  group('凭据怎么上线', () {
    test('普通请求走 Authorization 头，绝不进查询串', () async {
      final rec = _Recorder();
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:8080',
        token: 'deadbeef',
        client: rec.client((_) async => _json({'sessions': []})),
      );
      addTearDown(api.dispose);

      await api.sessions();

      final request = rec.requestTo('/sessions');
      expect(request.headers['authorization'], 'Bearer deadbeef');
      expect(
        request.url.toString(),
        isNot(contains('deadbeef')),
        reason:
            '长期 token 进 URL 就会进 access log、反代日志与浏览器历史 —— '
            '短命票据存在的全部理由就是避免这件事',
      );
    });

    test('POST /chat 也带头，而不是退化成票据', () async {
      final rec = _Recorder();
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:8080',
        token: 'deadbeef',
        client: rec.client(
          (_) async =>
              http.Response('data: {"type":"done","episode_id":"e1"}\n\n', 200),
        ),
      );
      addTearDown(api.dispose);

      await api.chat(sessionId: 's1', message: 'hi').toList();

      final request = rec.requestTo('/chat');
      expect(request.headers['authorization'], 'Bearer deadbeef');
      expect(request.url.query, isEmpty);
    });

    test('直传对象存储时**不带** cortexd 的 token', () async {
      final rec = _Recorder();
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:8080',
        token: 'deadbeef',
        client: rec.client((_) async => http.Response('', 200)),
      );
      addTearDown(api.dispose);

      await api.putPresigned(
        url: 'https://bucket.example/abc',
        bytes: Uint8List.fromList([1, 2, 3]),
      );

      final request = rec.requests.last;
      expect(request.url.host, 'bucket.example');
      expect(
        request.headers['authorization'],
        isNull,
        reason:
            '那是另一方的服务。把开整个记忆库的凭据发给对象存储既是泄露，'
            '也会因为 S3 的签名覆盖头集合而直接把上传弄失败',
      );
    });

    test('没有 token 时不伪造一个空的 Authorization 头', () async {
      final rec = _Recorder();
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:8080',
        client: rec.client((_) async => _json({'sessions': []})),
      );
      addTearDown(api.dispose);

      await api.sessions();
      expect(
        rec.requestTo('/sessions').headers.containsKey('authorization'),
        isFalse,
        reason: '`CORTEX_AUTH=disabled` 的 daemon 上一个空 Bearer 只会制造噪音',
      );
    });
  });

  group('票据', () {
    test('换一次票，60 秒内复用同一张', () async {
      final rec = _Recorder();
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:8080',
        token: 'deadbeef',
        client: rec.client(
          (_) async => _json({'ticket': 't-1', 'expires_in_secs': 60}),
        ),
      );
      addTearDown(api.dispose);

      final first = await api.issueTicket();
      expect(first.value, 't-1');
      expect(first.isUsableAt(DateTime.now()), isTrue);
      expect(
        first.isUsableAt(DateTime.now().add(const Duration(seconds: 55))),
        isFalse,
        reason: '留够余量：握手途中过期会变成一个 401，而 WS 握手拿不到状态码去解释它',
      );
    });

    test('WS 连接前先换票，且票据进的是查询串', () async {
      final rec = _Recorder();
      final api = HttpCortexApi(
        // 连不上的端口：WebSocket 握手必然失败，但那发生在换票之后 ——
        // 这里要钉的就是「先换票再连」这个顺序
        baseUrl: 'http://127.0.0.1:1',
        token: 'deadbeef',
        client: rec.client(
          (_) async => _json({'ticket': 't-9', 'expires_in_secs': 60}),
        ),
      );
      addTearDown(api.dispose);

      await expectLater(api.watchSync().toList(), throwsA(isA<Exception>()));

      expect(
        rec.requests.map((r) => r.url.path),
        contains('/auth/ticket'),
        reason: 'EventSource / WebSocket / <img> 加不了请求头，这是硬限制',
      );
    });

    test('没有 token 就不换票 —— 认证关着的 daemon 不需要', () async {
      final rec = _Recorder();
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:1',
        client: rec.client((_) async => _json({'ticket': 'x'})),
      );
      addTearDown(api.dispose);

      await expectLater(api.watchSync().toList(), throwsA(isA<Exception>()));
      expect(
        rec.requests.map((r) => r.url.path),
        isNot(contains('/auth/ticket')),
        reason: '向一个不检查凭据的 daemon 要票只会把一条本来能通的连接弄失败',
      );
    });
  });

  group('401 自愈', () {
    test('任何一条路上的 401 都会打回登录态', () async {
      var fired = 0;
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:8080',
        token: 'stale',
        client: MockClient((_) async => _json({'error': '未认证'}, status: 401)),
        onUnauthorized: () => fired++,
      );
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
      // 回调是延后一个 microtask 触发的：它会导致本实例被 dispose
      await Future<void>.delayed(Duration.zero);
      expect(fired, 1, reason: '过期必须在它发生的**任何**一条路上被发现，而不是只在被记得的那几条上');
    });

    test('401 的消息不区分「没带」与「带错」', () {
      const e = CortexApiException('未认证', statusCode: 401);
      expect(e.isUnauthorized, isTrue);
      expect(
        e.isUnsupported,
        isFalse,
        reason: '401 不是「这个 daemon 没有这条路由」，不能走降级到本地改动的那条分支',
      );
    });
  });

  group('登录闸门的状态机', () {
    test('服务端关着认证时直接放行，不问 token', () async {
      final container = _boot(_GateApi(auth: 'disabled'));
      addTearDown(container.dispose);

      await _settle(container);
      final state = container.read(authControllerProvider);
      expect(state.phase, AuthPhase.ready);
      expect(state.serverAuthDisabled, isTrue);
    });

    /// **「我改回官方服务器了，为什么还是离线」** —— 2026-08-30 实报。
    ///
    /// 离线是开机探测失败时自动兜上的，而它一旦为真 `probe()` 就在短路上
    /// 直接返回。改地址此前完全不碰这个标记，于是那个动作是空转的：
    /// 地址真换了、也真重探了，探测却停在短路上，界面一动不动。
    test('改地址会解除离线 —— 否则改了也一动不动', () async {
      final container = _boot(_GateApi(healthThrows: true));
      addTearDown(container.dispose);
      await _settle(container);
      expect(
        container.read(appConfigProvider).offline,
        isTrue,
        reason: '前提：开机连不上，先落进离线',
      );

      container
          .read(appConfigProvider.notifier)
          .setBaseUrl('http://127.0.0.1:9999');
      await _settle(container);

      expect(
        container.read(appConfigProvider).offline,
        isFalse,
        reason:
            '显式给出一个地址就是「连这里」。不解除的话，用户唯一的出路是'
            '聊天页横幅上那个「去连接」—— 而一个知道自己连错环境的人'
            '会去改地址，不会去点那里',
      );
      expect(
        container.read(authControllerProvider).phase,
        AuthPhase.unreachable,
        reason:
            '这一条是**正对照**：探测必须真的打出去了。停在离线短路上的话'
            'phase 会是 ready，而那正是这个 bug 的表现 —— 看起来什么都没发生',
      );
      expect(
        container.read(authControllerProvider).error,
        isNotNull,
        reason:
            '这一次是他主动去连的，失败原因要摆在他正在改的输入框上面，'
            '而不是再一次静默变回离线',
      );
    });

    /// 同一个地址不能短路掉：开机那一刻服务器恰好抽风的人，地址本来就是
    /// 对的，他能做的「重新连一下」正是原样再给一次。
    test('人在离线时，重新给一次同样的地址也要放行', () async {
      final container = _boot(_GateApi(healthThrows: true));
      addTearDown(container.dispose);
      await _settle(container);
      expect(container.read(appConfigProvider).offline, isTrue);

      final same = container.read(appConfigProvider).baseUrl;
      container.read(appConfigProvider.notifier).setBaseUrl(same);
      await _settle(container);

      expect(
        container.read(appConfigProvider).offline,
        isFalse,
        reason:
            '按地址短路等于把「地址本来就没错」的那个人锁死在离线里 —— '
            '他改成别的再改回来才出得去，而没有人猜得到要这么做',
      );
    });

    test('服务端开着认证而手上没有凭据时停在登录态', () async {
      final container = _boot(_GateApi());
      addTearDown(container.dispose);

      await _settle(container);
      expect(
        container.read(authControllerProvider).phase,
        AuthPhase.needsToken,
      );
    });

    test('开机连不上就落进离线模式，不拦一张登录表单', () async {
      final container = _boot(_GateApi(healthThrows: true));
      addTearDown(container.dispose);

      await _settle(container);
      final state = container.read(authControllerProvider);

      expect(
        state.phase,
        AuthPhase.ready,
        reason:
            '首次运行时地址是编译期默认值，那儿多半什么都没有。拦一张'
            '「登录一个不存在的服务器」的表单，等于让新用户在见到产品之前'
            '先卡住 —— 他没有账号、没有地址，也不知道该填什么',
      );
      expect(
        container.read(appConfigProvider).offline,
        isTrue,
        reason:
            '**必须同时开离线**。不开的话进去的是个坏掉的界面：'
            'localAgentOriginProvider 那条判据是「没 token 且服务端要 token 就'
            '不起 agent」，而连不上时 requiresToken 缺省为 true —— 本地 agent '
            '根本不启动，用户面对一个既没记忆也没工具的空壳',
      );
      expect(
        state.error,
        isNull,
        reason:
            '此刻那句「Connection refused」对用户没有可做的事 —— 他还没说'
            '他想连。等他点了横幅上的「去连接」，探测重来、失败落进 '
            'unreachable，那句话才出现在他正要改的那两个输入框上面。'
            '消息要在它有用的那一刻出现，而不是一直挂着',
      );
    });

    test('主动去连却连不上，仍然停在表单上', () async {
      final container = _boot(_GateApi(healthThrows: true));
      addTearDown(container.dispose);
      await _settle(container);

      // 用户在设置里改回在线，然后填了个连不上的地址按登录
      container.read(appConfigProvider.notifier).setOffline(false);
      await _settle(container);

      expect(
        container.read(authControllerProvider).phase,
        AuthPhase.unreachable,
        reason:
            '这一档剩下的含义很窄也更准：**你填了地址、按了登录、没通上**。'
            '那时停在表单上是对的 —— 他正要改的就是那两个输入框。'
            '开机探测那一档已经不走这里了',
      );
    });

    test('token 被接受后进入 ready，凭据交给 cortexApiProvider', () async {
      final api = _GateApi();
      final container = _boot(api);
      addTearDown(container.dispose);

      await _settle(container);
      await container.read(authControllerProvider.notifier).signIn('good');

      final state = container.read(authControllerProvider);
      expect(state.phase, AuthPhase.ready);
      expect(state.token, 'good');
      expect(
        api.ticketCalls,
        1,
        reason:
            'POST /auth/ticket 是最便宜的「这 token 认不认」探针：'
            '带认证、体积极小、副作用只是一张 60 秒后自灭的票',
      );
    });

    test('token 被拒时回到 needsToken 并丢掉它', () async {
      final container = _boot(_GateApi(rejectToken: true));
      addTearDown(container.dispose);

      await _settle(container);
      await container.read(authControllerProvider.notifier).signIn('bad');

      final state = container.read(authControllerProvider);
      expect(state.phase, AuthPhase.needsToken);
      expect(state.token, isNull);
      // 这句以前断言的是「摘要」——那段文案专讲预共享 token（「别把摘要当
      // 明文填」）。token 那条路已经从登录页上拿掉了，而这段代码现在只在
      // **存下来的凭据过期**时走到，指路当然要换成那件事的下一步
      expect(
        state.error,
        contains('重新登录'),
        reason: '要说清下一步做什么；讲一个用户从来没用过的 token 形态是白指路',
      );
    });

    test('签发端点连不上时不算「token 不对」', () async {
      final container = _boot(_GateApi(ticketThrowsTransport: true));
      addTearDown(container.dispose);

      await _settle(container);
      await container.read(authControllerProvider.notifier).signIn('good');

      expect(
        container.read(authControllerProvider).phase,
        AuthPhase.unreachable,
      );
    });

    test('运行中收到 401 会打回登录态', () async {
      final container = _boot(_GateApi());
      addTearDown(container.dispose);
      await _settle(container);
      await container.read(authControllerProvider.notifier).signIn('good');
      expect(container.read(authControllerProvider).phase, AuthPhase.ready);

      container.read(authControllerProvider.notifier).onUnauthorized();

      final state = container.read(authControllerProvider);
      expect(state.phase, AuthPhase.needsToken);
      expect(state.token, isNull);
      expect(
        state.error,
        contains('重新登录'),
        reason:
            '从 ready 掉下来才有红字，且不再引用状态码 —— '
            '「HTTP 401」在登录表单上读起来像密码被拒',
      );
    });

    test('环境变量里的 token 会被自动用掉，用户根本看不到登录屏', () async {
      final api = _GateApi();
      final container = _boot(api, seed: 'from-env');
      addTearDown(container.dispose);

      await _settle(container);

      final state = container.read(authControllerProvider);
      expect(state.phase, AuthPhase.ready);
      expect(state.token, 'from-env');
      expect(
        api.lastToken,
        'from-env',
        reason:
            '「已经配好了」的机器不该被再问一次 —— 而手上有 token 也仍然要被'
            '服务端**接受**过才算数：轮换过的旧 token 看起来和好 token 一模一样',
      );
    });

    test('环境变量里的 token 被拒时，落到登录屏而不是死在半路', () async {
      final container = _boot(_GateApi(rejectToken: true), seed: 'rotated');
      addTearDown(container.dispose);

      await _settle(container);

      final state = container.read(authControllerProvider);
      expect(state.phase, AuthPhase.needsToken);
      expect(state.token, isNull);
    });

    test('mock 数据源不走网络也不问 token', () async {
      final container = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          authSeedTokenProvider.overrideWithValue(null),
          authProbeApiProvider.overrideWithValue(
            (token) => throw StateError('mock 模式下不该探测任何 daemon'),
          ),
        ],
      );
      addTearDown(container.dispose);

      await _settle(container);
      expect(container.read(authControllerProvider).phase, AuthPhase.ready);
    });
  });
}

/// Boots a container whose gate probes [api] instead of a socket.
///
/// [seed] is pinned rather than inherited from the real environment: the setup
/// this project recommends (`setx CORTEXD_TOKEN …`) would otherwise make the
/// "no credential in hand" cases pass on CI and fail on a developer's machine.
ProviderContainer _boot(CortexApi api, {String? seed}) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_LiveConfig.new),
    authSeedTokenProvider.overrideWithValue(seed),
    authProbeApiProvider.overrideWithValue((token) {
      if (api is _GateApi) api.lastToken = token;
      return api;
    }),
  ],
);

/// 排干微任务，直到 [cond] 成立（或者放弃）。
///
/// ⚠️ **不要换回「排固定次数的 `Future.delayed(Duration.zero)`」。**
/// 那种写法把测试钉死在实现此刻的 await 次数上：2026-08-19 给启动路径
/// 加了一次「等地址落定」的 await（修「每次启动都要重新登录」那个 race），
/// 三条本来无关的测试当场变红 —— 它们等的圈数不够，而失败信息说的是
/// 「前提没成立」，看起来像业务坏了。
Future<void> _settleUntil(bool Function() cond, {int rounds = 60}) async {
  for (var i = 0; i < rounds && !cond(); i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

/// Drives the deferred probe to completion.
Future<void> _settle(ProviderContainer container) async {
  container.read(authControllerProvider);
  await _settleUntil(
    () => container.read(authControllerProvider).phase != AuthPhase.probing,
  );
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

/// A daemon stand-in for the gate: only `/health` and `/auth/ticket` matter.
class _GateApi
    with
        ModelSourcesUnsupported,
        AccountUnsupported,
        LocalWorkspaceUnsupported,
        LocalMcpUnsupported,
        RunAttachUnsupported,
        ImagesUnsupported,
        AssistantsUnsupported,
        SkillsUnsupported,
        LlmModelsUnsupported,
        SessionSearchUnsupported,
        UsageUnsupported,
        SandboxHealthUnsupported,
        AgentsUnsupported,
        LocalAttachUnsupported
    implements CortexApi {
  _GateApi({
    this.auth = 'token',
    this.healthThrows = false,
    this.rejectToken = false,
    this.ticketThrowsTransport = false,
  });

  final String auth;
  final bool healthThrows;
  final bool rejectToken;
  final bool ticketThrowsTransport;

  String? lastToken;
  int ticketCalls = 0;

  /// 没有本地 agent 的替身：这条路由不存在，报「不支持」正是让调用方
  /// 去走 `PATCH /sessions/{id}` 回落分支的那个信号。
  /// 测试替身不做导入 —— 报「不支持」，让调用方走它的降级分支。
  @override
  Future<AuthTokens> login(String username, String password) async {
    throw const CortexApiException('这个数据源不支持账号登录', statusCode: 501);
  }

  @override
  Future<AuthTokens> register(String username, String password) async {
    throw const CortexApiException('这个数据源不支持账号注册', statusCode: 501);
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
  Future<HealthStatus> health() async {
    if (healthThrows) {
      throw const CortexApiException('连不上 cortexd');
    }
    return HealthStatus(status: 'ok', version: 't', database: 'ok', auth: auth);
  }

  @override
  Future<AuthTicket> issueTicket() async {
    ticketCalls++;
    if (ticketThrowsTransport) {
      throw const CortexApiException('连不上 cortexd');
    }
    if (rejectToken) {
      throw const CortexApiException('未认证', statusCode: 401);
    }
    return AuthTicket(
      value: 't',
      expiresAt: DateTime.now().add(const Duration(seconds: 60)),
    );
  }

  @override
  String get label => 'gate-fake';

  @override
  void dispose() {}

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
  }) => throw UnimplementedError('闸门只该碰 /health 与 /auth/ticket');

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) => throw UnimplementedError('闸门只该碰 /health 与 /auth/ticket');

  @override
  Future<List<Project>> projects() =>
      throw UnimplementedError('闸门只该碰 /health 与 /auth/ticket');

  @override
  Future<Project> createProject(String name) => throw UnimplementedError();

  @override
  Future<Project> patchProject(String id, {String? name, bool? pinned}) =>
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
    bool? pinned,
    String? workspace,
    bool clearWorkspace = false,
  }) => throw UnimplementedError();

  @override
  Future<ChatSession> forkSession(String id, {String? upToEpisodeId}) =>
      throw UnimplementedError();

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
  Future<Uint8List> exportSessions() async =>
      throw UnimplementedError('这个替身不测会话导出');

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
  Future<SyncPage> sync({required int since, int limit = 500}) =>
      throw UnimplementedError();
}

/// 记住 / 换回一对新令牌用的假后端。
class _SessionApi extends _GateApi {
  _SessionApi({this.refreshFails = false, this.refreshStatusCode = 401})
    : super();

  /// 可变：有的测试要「先续得上、后来续不上」——
  /// 服务端发版重启正是这个时序
  bool refreshFails;

  /// 续期失败时抛的状态码。401 = 凭据被拒；5xx / 429 = 服务端的问题
  int refreshStatusCode;
  int loginCalls = 0;
  int refreshCalls = 0;
  int logoutCalls = 0;
  String? lastRefreshSent;

  AuthTokens _tokens(String suffix) => AuthTokens(
    accessToken: 'access-$suffix',
    accessExpiresInSecs: 900,
    refreshToken: 'refresh-$suffix',
    refreshExpiresInSecs: 2592000,
    userId: 'u1',
  );

  @override
  Future<AuthTokens> login(String username, String password) async {
    loginCalls++;
    return _tokens('1');
  }

  @override
  Future<AuthTokens> refreshSession(String refreshToken) async {
    refreshCalls++;
    lastRefreshSent = refreshToken;
    if (refreshFails) {
      throw CortexApiException('续期没成', statusCode: refreshStatusCode);
    }
    return _tokens('2');
  }

  @override
  Future<void> logout(String refreshToken) async {
    logoutCalls++;
    lastRefreshSent = refreshToken;
  }
}

/// 探针可以按开关拒绝 —— 白续裁决那组用例要「续期一直成功、直连探测
/// 却各有说法」这个组合，[_SessionApi] 自己给不出来。
class _FutileProbeApi extends _SessionApi {
  bool ticketRejects = false;

  @override
  Future<AuthTicket> issueTicket() {
    if (ticketRejects) {
      throw const CortexApiException('未认证', statusCode: 401);
    }
    return super.issueTicket();
  }
}

void _resilienceTests() {
  group('续期的计账与裁决', () {
    /// **429 不消耗 transient 预算。**
    ///
    /// 发版重启风暴里 5xx 与自家限流的 429 交替出现：全体客户端同时涌向
    /// /auth/refresh，按 token 摘要的每分钟 5 次限流必然掐掉一部分。429
    /// 混进 3 次的 transient 预算里，一次不算长的抖动就把人送回登录页。
    test('连续 429：不吃 transient 预算，前 7 次都不踢', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await ctrl.restoreSession('refresh-old');
      expect(c.read(authControllerProvider).isReady, isTrue);

      api.refreshFails = true;
      api.refreshStatusCode = 429;

      // 老行为下第 3 次就 gate（429 被记成 transient）—— 这里 7 次都得稳住
      for (var i = 1; i <= 7; i++) {
        await ctrl.onUnauthorized();
        expect(
          c.read(authControllerProvider).isReady,
          isTrue,
          reason:
              '第 $i 次 429 就被踢了 —— 429 是「等等再来」，不是服务端抖动，'
              '混进 3 次的 transient 预算等于发版重启必踢人',
        );
      }

      // 但它不是免死金牌：自己的预算（8 次）用完之后**歇一会儿** ——
      // 出口是「停止重试」，不是「把人踢出去」
      await ctrl.onUnauthorized();
      final st = c.read(authControllerProvider);
      expect(
        st.isReady,
        isTrue,
        reason:
            '429 是「等等再来」，把人踢回登录页等于让服务端的一次忙碌'
            '销毁用户敲了一半的话',
      );
      expect(
        st.refreshToken,
        isNotNull,
        reason: '429 不是凭据的错，不许删凭据 —— 下一条 401 就能续上',
      );
      final calls = api.refreshCalls;
      await ctrl.onUnauthorized();
      expect(
        api.refreshCalls,
        calls,
        reason:
            '永远 429 的续期循环必须有出口，否则就是台永动机 —— '
            '出口是歇着，不是登出',
      );
    });

    /// **白续收口前先裁决：直连通就重启 agent，不登出。**
    ///
    /// 桌面端的请求全经本机 agent 反代 —— 它的转发链坏掉时，远端 401 与
    /// 「服务端不认新凭据」在客户端看来一模一样，而续期走直连、次次成功。
    /// 老行为把这一族一律判成 refreshFutile 踢回登录页：**误杀**，因为
    /// 服务端明明认这把凭据。裁决探测（直连打 /auth/ticket）把两族分开。
    test('白续三次但直连裁决通过：重启 agent，不掉登录页', () async {
      final api = _FutileProbeApi();
      var agentBuilds = 0;
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          // 假装本机 agent 在场 —— 裁决只有「请求经过 agent」时才有意义
          localAgentOriginProvider.overrideWith((ref) async {
            agentBuilds++;
            return 'http://127.0.0.1:9';
          }),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await c.read(localAgentOriginProvider.future);
      await ctrl.restoreSession('refresh-old');
      expect(c.read(authControllerProvider).isReady, isTrue);

      // 拨表：出 3 秒 cooloff、留在 60 秒 futile 窗口内
      var now = DateTime(2026, 8, 25, 12, 0, 0);
      ctrl.clock = () => now;

      await ctrl.onUnauthorized(); // 第一次：正常续上
      for (var round = 1; round <= 3; round++) {
        now = now.add(const Duration(seconds: 5));
        await ctrl.onUnauthorized(); // 续期又「成功」—— 白续
      }

      expect(
        c.read(authControllerProvider).isReady,
        isTrue,
        reason:
            '直连裁决通过 = 服务端认这把新凭据，坏的是本机 agent 链路 —— '
            '此时回登录页是误杀（正是「每 15 分钟被踢」的收口点）',
      );
      // invalidate 是懒生效的：下一次读才真的重建
      await c.read(localAgentOriginProvider.future);
      expect(
        agentBuilds,
        2,
        reason: '裁决通过必须重启一次本机 agent —— 不重启的话链路还是坏的，下一轮照旧白续',
      );
    });

    /// 裁决探测也 401 —— 真是服务端不认，照旧收口回登录页。
    ///
    /// 这半边不能丢：futile 熔断防的「多实例验签不同步」真实存在
    /// （AccessBook 在内存），裁决只是把误杀分出去，不是把熔断删掉。
    test('白续三次且直连也 401：照旧收口，agent 不背锅', () async {
      final api = _FutileProbeApi()..ticketRejects = true;
      var agentBuilds = 0;
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          localAgentOriginProvider.overrideWith((ref) async {
            agentBuilds++;
            return 'http://127.0.0.1:9';
          }),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await c.read(localAgentOriginProvider.future);
      await ctrl.restoreSession('refresh-old');

      var now = DateTime(2026, 8, 25, 12, 0, 0);
      ctrl.clock = () => now;

      await ctrl.onUnauthorized();
      for (var round = 1; round <= 3; round++) {
        now = now.add(const Duration(seconds: 5));
        await ctrl.onUnauthorized();
      }

      final st = c.read(authControllerProvider);
      expect(
        st.phase,
        AuthPhase.needsToken,
        reason: '直连也不认 = 病根在服务端，熔断必须照旧收口 —— 删掉它那族回到静默坏死',
      );
      expect(st.error, contains('配置'), reason: '红字要指向服务端，不是让用户怀疑密码');
      await c.read(localAgentOriginProvider.future);
      expect(agentBuilds, 1, reason: '服务端的问题不许拿 agent 撒气 —— 重启治不了它');
    });
  });
}

void _registerTests() {
  group('注册', () {
    /// 注册即登录：拿回的令牌直接进主界面，不再让用户「现在去登录」。
    test('注册成功后自动登录，refresh token 已在手上', () async {
      final api = _RegisterApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await ctrl.signUpWithPassword('newbie', 'hunter2-hunter2');

      final st = c.read(authControllerProvider);
      expect(st.phase, AuthPhase.ready, reason: '注册成功就该直接进主界面');
      expect(st.token, 'access-1');
      expect(
        st.refreshToken,
        'refresh-1',
        reason: '注册换回的 refresh token 必须留在手上 —— 否则第一次 401 就被踢',
      );
      expect(api.registerCalls, 1);
    });

    /// 部署没开注册时，403 的原话（含管理员怎么开）要到用户眼前。
    test('注册被 403 拒时，服务端的原话原样显示', () async {
      final api = _RegisterApi()..rejectRegister = true;
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await ctrl.signUpWithPassword('newbie', 'hunter2-hunter2');

      final st = c.read(authControllerProvider);
      expect(st.isReady, isFalse);
      expect(
        st.error,
        contains('没有开放注册'),
        reason: '403 正文写着管理员该怎么开 —— 吞掉它用户只能来问我们',
      );
    });

    /// `open_registration` 是三态：缺字段是 null（要去 /sandbox/health
    /// 补问），不是 false（那会把生产上开着注册的部署静默藏掉入口）。
    test('health 的 open_registration 解析成三态', () {
      expect(
        HealthStatus.fromJson({
          ..._health,
          'open_registration': true,
        }).openRegistration,
        isTrue,
      );
      expect(
        HealthStatus.fromJson({
          ..._health,
          'open_registration': false,
        }).openRegistration,
        isFalse,
      );
      expect(
        HealthStatus.fromJson(_health).openRegistration,
        isNull,
        reason:
            '缺字段 ≠ 关。生产边缘把 /health 分给了记忆服务，那份响应里没有'
            '这个字段 —— 压成 false 的话，开着注册的生产部署永远不出现入口',
      );
    });
  });
}

/// 注册用的假后端。
class _RegisterApi extends _GateApi {
  int registerCalls = 0;
  bool rejectRegister = false;

  @override
  Future<AuthTokens> register(String username, String password) async {
    registerCalls++;
    if (rejectRegister) {
      throw const CortexApiException(
        '这个部署没有开放注册。管理员可以设 CORTEX_OPEN_REGISTRATION=enabled 打开。',
        statusCode: 403,
      );
    }
    return AuthTokens(
      accessToken: 'access-1',
      accessExpiresInSecs: 900,
      refreshToken: 'refresh-1',
      refreshExpiresInSecs: 2592000,
      userId: 'u-new',
    );
  }
}

void _sessionTests() {
  group('会话持久', () {
    /// **续期必须存下新的那个。**
    ///
    /// 服务端每次刷新都轮转：旧的立刻作废。客户端存了旧的，下次启动就会
    /// 拿一个已经作废的去换 —— 而那被判成**重放**，后果是整条链一起失效，
    /// 用户看到的是「莫名其妙被登出，而且这次连密码都白输了」。
    test('续期之后存下来的是新令牌，不是旧的', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          // **必须替掉**：不替的话这条测试会去读开发机上真实的
          // `%APPDATA%\cortex\settings.json`。里面那个地址一被读回来就会
          // 触发 `_reset()`，把这次续期判成「已被取代」——
          // 于是测试的成败取决于跑它的那台机器上存过什么地址
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      // 控制器 build 时会排一次探活，而那会递增 generation。不等它落定就
      // 调用，我们这次的 generation 会被它作废，返回值变成「没续上」——
      // 那是测试的时序问题，不是代码的
      await _settleUntil(
        () => c.read(authControllerProvider).phase != AuthPhase.probing,
      );
      final outcome = await ctrl.restoreSession('refresh-old');

      expect(outcome, RefreshOutcome.ok);
      expect(api.lastRefreshSent, 'refresh-old', reason: '换的时候要用旧的');
      expect(
        c.read(authControllerProvider).refreshToken,
        'refresh-2',
        reason: '**存下来的必须是新的** —— 存旧的会在下次启动被判成重放，整条链一起废',
      );
      expect(c.read(authControllerProvider).token, 'access-2');
    });

    /// 凭据真的失效时落回登录页，而不是卡在半路。
    test('续期失败就落回登录页', () async {
      final api = _SessionApi(refreshFails: true);
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          // **必须替掉**：不替的话这条测试会去读开发机上真实的
          // `%APPDATA%\cortex\settings.json`。里面那个地址一被读回来就会
          // 触发 `_reset()`，把这次续期判成「已被取代」——
          // 于是测试的成败取决于跑它的那台机器上存过什么地址
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      final outcome = await ctrl.restoreSession('refresh-dead');
      expect(
        outcome,
        RefreshOutcome.rejected,
        reason: '401 是服务端对凭据的明确裁决 —— 只有这一档才配删存储里的副本',
      );
    });

    /// **5xx 不是凭据的错，一个字节都不许动。**
    ///
    /// 老代码「只要不是连不上就删凭据」——于是发版重启那几秒里的 502、
    /// 自家限流的 429、打到无账号后端的 501，都把 30 天的凭据当场删掉，
    /// 用户被踢回登录页且下次启动再也自动续不上。「过一会儿突然退出」
    /// 的一半来源就是它。
    test('续期遇到 502：transient，凭据不删', () async {
      final api = _SessionApi(refreshFails: true, refreshStatusCode: 502);
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      final outcome = await ctrl.restoreSession('refresh-old');
      expect(
        outcome,
        RefreshOutcome.transient,
        reason: '502 是服务端此刻答不上来，不是凭据被拒 —— 判据错了凭据就被误删',
      );
    });

    /// **Web 形态（seed 恒 null + storage 里有 refresh token）下，启动必须走续期。**
    ///
    /// 这条钉的是一个存在过的整链断裂：Web 版的 `readSeedToken` 曾经与
    /// `readRememberedToken` 读同一个 sessionStorage key。于是
    /// `_bootstrap` 里「remembered ≠ seed 才续期」恒假 ——
    /// `restoreSession` 在 Web 上**从来没有被调用过**，「登录一次管 30 天」
    /// 在 Web 上根本不存在；同时 refresh token 被当预共享 token 塞进
    /// Authorization 头，启动即 401 红字。
    test('Web 形态：启动时用 remembered 续期并直接 ready', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          // **必须替掉**：不替的话这条测试会去读开发机上真实的
          // `%APPDATA%\cortex\settings.json`。里面那个地址一被读回来就会
          // 触发 `_reset()`，把这次续期判成「已被取代」——
          // 于是测试的成败取决于跑它的那台机器上存过什么地址
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          authSeedTokenProvider.overrideWithValue(null), // Web：无预共享 token
          rememberedTokenProvider.overrideWith((_) async => 'refresh-old'),
        ],
      );
      addTearDown(c.dispose);

      c.read(authControllerProvider); // 触发 build → bootstrap
      // bootstrap 全程 async：让微任务与 restoreSession 落定
      // 等到**真的 ready**，不是等到「发出去了」——
      // 后者在状态写回之前就成立了
      await _settleUntil(() => c.read(authControllerProvider).isReady);

      expect(
        api.refreshCalls,
        greaterThan(0),
        reason:
            '启动时必须真的去续期。以前 seed 与 remembered 是同一个值，'
            '「不同才续」的判断恒假，restoreSession 全项目零调用',
      );
      final st = c.read(authControllerProvider);
      expect(st.isReady, isTrue, reason: '续期成功就该直接进主界面，不见登录页');
      expect(st.token, 'access-2');
    });

    /// 启动时旧凭据续不上 —— 安静回登录页，**不带红字**。
    ///
    /// 打开页面时揣着一份过期凭据是常态（服务端重启过、库清过、30 天到了）。
    /// 「凭据已失效（HTTP 401）」压在还没输过密码的表单上，读起来是
    /// 「我被拒绝了」—— 用户会把一次正常的登录当成失败来排查。实际发生过。
    test('启动续期失败：回登录页且不显示 401 红字', () async {
      final api = _SessionApi(refreshFails: true);
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          // **必须替掉**：不替的话这条测试会去读开发机上真实的
          // `%APPDATA%\cortex\settings.json`。里面那个地址一被读回来就会
          // 触发 `_reset()`，把这次续期判成「已被取代」——
          // 于是测试的成败取决于跑它的那台机器上存过什么地址
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          authSeedTokenProvider.overrideWithValue(null),
          rememberedTokenProvider.overrideWith((_) async => 'refresh-dead'),
        ],
      );
      addTearDown(c.dispose);

      c.read(authControllerProvider);
      for (var i = 0; i < 8; i++) {
        await Future<void>.delayed(Duration.zero);
      }

      final st = c.read(authControllerProvider);
      expect(st.isReady, isFalse);
      expect(st.error, isNull, reason: '启动清场不是事故，登录页本身已经在说「请登录」了');
    });

    /// **半路 401 先续期，而不是把人弹回登录框。**
    ///
    /// access token 只活 15 分钟，服务端每重启一次那本簿子就清空一次
    /// （它在内存里）。原来的做法是一收到 401 就回登录页 —— 而用户手上
    /// 明明揣着一张 30 天的 refresh token。生产上每发一次版，所有在线的人
    /// 当场掉线。
    test('半路 401 会先拿 refresh token 续，不弹登录框', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          // **必须替掉**：不替的话这条测试会去读开发机上真实的
          // `%APPDATA%\cortex\settings.json`。里面那个地址一被读回来就会
          // 触发 `_reset()`，把这次续期判成「已被取代」——
          // 于是测试的成败取决于跑它的那台机器上存过什么地址
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await ctrl.restoreSession('refresh-old'); // 先有一份可用的凭据
      expect(c.read(authControllerProvider).isReady, isTrue);

      await ctrl.onUnauthorized();

      expect(
        c.read(authControllerProvider).isReady,
        isTrue,
        reason: '续得上就该留在原地，把人弹回登录框是这次要修掉的行为',
      );
      expect(c.read(authControllerProvider).token, 'access-2');
    });

    /// **并发的 401 只能触发一次续期。**
    ///
    /// refresh token 一次性轮转，服务端把「拿一个已经轮转过的来换」判成泄露，
    /// 于是**整条 family 一起作废**。而 401 是成批来的（服务端一重启，页面上
    /// 七八个在飞的请求同时被拒）。每个各刷各的，第一个成功、其余全是重放 ——
    /// 这个人被彻底登出。
    ///
    /// 这条测试比上一条重要：写错了不是「体验差一点」，是**主动把用户踢下线**。
    test('并发的 401 只刷一次，不会被服务端判成重放', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          // **必须替掉**：不替的话这条测试会去读开发机上真实的
          // `%APPDATA%\cortex\settings.json`。里面那个地址一被读回来就会
          // 触发 `_reset()`，把这次续期判成「已被取代」——
          // 于是测试的成败取决于跑它的那台机器上存过什么地址
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await ctrl.restoreSession('refresh-old');
      final before = api.refreshCalls;

      // 七个请求同时撞上 401 —— **同一轮里全部发起**，这正是服务端重启时
      // 页面上那几个在飞的请求的样子
      await Future.wait([for (var i = 0; i < 7; i++) ctrl.onUnauthorized()]);

      expect(
        api.refreshCalls - before,
        1,
        reason:
            '七次 401 只该换来一次续期。多出来的每一次都是拿已经轮转过的那份'
            '去换 —— 服务端看到的是重放，反手把整条 family 作废',
      );
    });

    /// **续期成功之后又 401 —— 不能再续，那是一场风暴。**
    ///
    /// 续成功之后客户端重建、各面板重新拉一遍。要是那之后立刻又 401，
    /// 说明新 token 也不好使 —— 再续只会重复同一个循环，而每圈都签一对新的
    /// refresh token。
    ///
    /// 这不是假想：加上「先续期」而没有这道闸之后，实测五分钟里签了 **2765**
    /// 对，页面因为每次状态变化重建整棵树而疯狂闪烁。这条测试要是早写了，
    /// 那次就不会发生。
    test('续期成功后紧接着的 401：不再续，也不踢 —— 当迟到毛刺忽略', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          // **必须替掉**：不替的话这条测试会去读开发机上真实的
          // `%APPDATA%\cortex\settings.json`。里面那个地址一被读回来就会
          // 触发 `_reset()`，把这次续期判成「已被取代」——
          // 于是测试的成败取决于跑它的那台机器上存过什么地址
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await ctrl.restoreSession('refresh-old');

      await ctrl.onUnauthorized(); // 第一次：续上了
      final afterFirst = api.refreshCalls;
      expect(c.read(authControllerProvider).isReady, isTrue);

      await ctrl.onUnauthorized(); // 紧接着又一次

      expect(
        api.refreshCalls,
        afterFirst,
        reason:
            '刚续过还 401，再续一次只会重复循环，而每圈都签一对新的 '
            'refresh token —— 那正是那场 2765 对的风暴。这半边不许回退',
      );
      // ⚠️ 2026-08-24 反转：老行为是直接 _fallBackToGate 踢回登录页。
      // 但新 token 生效后的头几秒里，重建窗口的迟到毛刺、负载均衡后面
      // 一台还没跟上的旧实例都会掉出一两条 401 —— 按老行为，每 15 分钟
      // （access 的寿命）就有一次被误杀的机会，用户的症状是「开着不动
      // 过一会儿就被退出」。真坏的凭据不会漏网：窗口一过，下一轮续期
      // 由服务端拒绝（refreshRejected）收口
      expect(
        c.read(authControllerProvider).isReady,
        isTrue,
        reason: '冷却窗口内的 401 是迟到毛刺，不构成登出的理由',
      );
      expect(
        c.read(authControllerProvider).error,
        isNull,
        reason: '没有踢人就不许有红字',
      );
    });

    /// **一次失败的续期只结一次账 —— 有几个等待者都一样。**
    ///
    /// 审查抓到的：结账曾写在每个 awaiter 手里，「服务端一重启，七八个
    /// 在飞请求同时 401」这个常态下，同一次 502 被七个等待者各记一笔，
    /// 「连续 3 次才踢」实际是 1 次就踢 —— 发版重启踢人换了句红字回来了。
    /// 串行调用的测试踩不到它，这条必须并发。
    test('并发 7 条 401 共享一次 502 续期：预算只记一次，不踢', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await ctrl.restoreSession('refresh-old');
      expect(c.read(authControllerProvider).isReady, isTrue);

      api.refreshFails = true;
      api.refreshStatusCode = 502;
      final before = api.refreshCalls;

      // 服务端重启：一批在飞请求同时被拒
      await Future.wait([for (var i = 0; i < 7; i++) ctrl.onUnauthorized()]);

      expect(
        api.refreshCalls,
        before + 1,
        reason: '七个等待者必须合流成一次续期 —— 各刷各的会被服务端判成重放',
      );
      expect(
        c.read(authControllerProvider).isReady,
        isTrue,
        reason:
            '一次 502 抖动只许消耗一格预算。按等待者计数的话，'
            '第 3 个等待者就把人踢了 —— 发版重启踢人根本没修掉',
      );

      // 后续两次独立的 transient 耗尽预算 —— 而**耗尽不等于踢人**
      await ctrl.onUnauthorized();
      expect(c.read(authControllerProvider).isReady, isTrue);
      await ctrl.onUnauthorized();
      expect(
        c.read(authControllerProvider).isReady,
        isTrue,
        reason:
            '⚠️ 预算烧完把人踢回了登录页。抖动是**服务端此刻答不上来**，'
            '凭据好好的（那条日志自己都写着「凭据留着，稍后能自动续」）'
            '—— 而回登录页会连用户敲了一半的话一起销毁',
      );

      // 预算防的是**无限重试**，那个仍然要成立：歇着的时候不再去撞
      final afterBudget = api.refreshCalls;
      await ctrl.onUnauthorized();
      expect(
        api.refreshCalls,
        afterBudget,
        reason: '烧完预算之后还在继续撞服务端 —— 那就是当初要防的永动机',
      );
    });

    /// **续期一直成功、可新 token 照样被拒 —— 必须有终态。**
    ///
    /// 这正是当年引入 cooloff 的那场生产风暴的形态（五分钟 2765 对：
    /// refresh 路由一直答应，access 一直 401）。cooloff 从「踢」改成
    /// 「忽略」之后，这一族一度失去了全部出口：不 rejected、不 stalled，
    /// 界面停在 ready、每 3 秒签一对、静默坏死。futile 预算是补回的终态。
    test('连续白续三次（ok 但新 token 没起作用）：收口回登录页', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      // 手上得先有一枚 refresh token，onUnauthorized 才有得续
      await ctrl.restoreSession('refresh-old');

      // 拨表：cooloff（3 秒）要过、futile 窗口（60 秒）不过 ——
      // 真等墙钟会把这条测试拖到十几秒
      var now = DateTime(2026, 8, 24, 12, 0, 0);
      ctrl.clock = () => now;

      await ctrl.onUnauthorized(); // 第一次：正常续上
      expect(c.read(authControllerProvider).isReady, isTrue);

      for (var round = 1; round <= 3; round++) {
        now = now.add(const Duration(seconds: 5)); // 出 cooloff、在 futile 窗口内
        await ctrl.onUnauthorized(); // 续期又「成功」—— 但这就是问题所在
      }

      final st = c.read(authControllerProvider);
      expect(
        st.phase,
        AuthPhase.needsToken,
        reason:
            '三对新 token 都没让 401 停下来，再签只是浪费 —— '
            '无终态的话这是每 3 秒签一对的静默坏死',
      );
      expect(
        st.error,
        contains('配置'),
        reason: '这族的病根在服务端（验签不同步 / 时钟偏差），红字要指对方向',
      );
    });

    /// **换人换后端不背旧账。**
    test('手动重新登录后，续期预算从零开始', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await ctrl.restoreSession('refresh-old');

      // 攒两次 transient（差一次就踢）
      api.refreshFails = true;
      api.refreshStatusCode = 502;
      await ctrl.onUnauthorized();
      await ctrl.onUnauthorized();
      expect(c.read(authControllerProvider).isReady, isTrue);

      // 用户手动重新登录成功
      api.refreshFails = false;
      await ctrl.signInWithPassword('u', 'p');
      expect(c.read(authControllerProvider).isReady, isTrue);

      // 新会话的第一次抖动不许直接踢 —— 旧账必须已清
      api.refreshFails = true;
      await ctrl.onUnauthorized();
      expect(
        c.read(authControllerProvider).isReady,
        isTrue,
        reason:
            '登录成功不清计数的话，上一段会话攒的 2 次会记到新会话头上，'
            '「连续 3 次」的承诺实际只剩 1 次',
      );
    });

    /// **服务端抖动不踢人，但抖个不停要有出口。**
    ///
    /// transient 不踢单独存在就是台永动机（服务端持续 5xx 时每条 401 都
    /// 「续一下 → 没成 → 算了」，循环永不收敛）—— 与「agent 被 1 秒一次
    /// 杀了 730 次」同形状：没有上限的自愈不是自愈。
    test('续期连续 transient：预算烧完也不踢人，只是歇一会儿', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await ctrl.restoreSession('refresh-old'); // 先正常登上
      expect(c.read(authControllerProvider).isReady, isTrue);

      // 服务端开始发版重启：续期一律 502
      api.refreshFails = true;
      api.refreshStatusCode = 502;

      await ctrl.onUnauthorized(); // 第 1 次 transient
      expect(
        c.read(authControllerProvider).isReady,
        isTrue,
        reason: '一次 502 不是登出的理由 —— 凭据没毛病，等服务端缓过来',
      );
      await ctrl.onUnauthorized(); // 第 2 次
      expect(c.read(authControllerProvider).isReady, isTrue);

      await ctrl.onUnauthorized(); // 第 3 次：预算用完
      final st = c.read(authControllerProvider);
      expect(
        st.isReady,
        isTrue,
        reason:
            '⚠️ **预算烧完不该踢人。** 2026-08-30 用户实报「输入会话内容，'
            '整个会话丢失，窗口闪一下全部重置」，诊断文件里正是这一条 '
            'refreshStalled，而它的 detail 自己写着「凭据留着，稍后能自动续」。'
            '停止重试与销毁用户的工作是两件事，预算只该做前一件',
      );
      expect(
        st.refreshToken,
        isNotNull,
        reason: '凭据必须留着：服务端缓过来之后下一条 401 就能无感续上',
      );

      // 而「防永动机」那一半仍然要成立：歇着的时候不再去撞服务端
      final calls = api.refreshCalls;
      await ctrl.onUnauthorized();
      await ctrl.onUnauthorized();
      expect(
        api.refreshCalls,
        calls,
        reason: '烧完预算之后还在一条 401 撞一次 —— 那就是当初立这道预算要防的',
      );
    });

    /// **未登录阶段的杂散 401 不许在登录页压红字。**
    ///
    /// 这条直接踩 `_fallBackToGate` 的非 ready 分支 —— 上一轮它「被修复过」
    /// 但脚本替换静默落空，而当时的测试都绕着它走，全绿。这次的断言就钉在
    /// 它写出来的 error 上。
    test('未登录时收到 401：回登录页但不写红字', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          // **必须替掉**：不替的话这条测试会去读开发机上真实的
          // `%APPDATA%\cortex\settings.json`。里面那个地址一被读回来就会
          // 触发 `_reset()`，把这次续期判成「已被取代」——
          // 于是测试的成败取决于跑它的那台机器上存过什么地址
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          authSeedTokenProvider.overrideWithValue(null),
          rememberedTokenProvider.overrideWith((_) async => null),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      for (var i = 0; i < 6; i++) {
        await Future<void>.delayed(Duration.zero);
      }
      expect(c.read(authControllerProvider).isReady, isFalse);

      // 登录页阶段某个后台请求 401 了（历史上是 sync 的 ticket）
      await ctrl.onUnauthorized();

      expect(
        c.read(authControllerProvider).error,
        isNull,
        reason:
            '「凭据已失效」压在一张还没输过密码的表单上，读起来是'
            '「我被拒绝了」—— 用户会把一次正常的登录当成失败来排查。实际发生过',
      );
    });

    /// **登出要让服务端作废，不能只删本地。**
    ///
    /// 只删本地是「这台机器忘了」，而已经泄露出去的那一份还能用到 30 天后。
    test('登出会把作废请求发给服务端', () async {
      final api = _SessionApi();
      final c = ProviderContainer(
        overrides: [
          authProbeApiProvider.overrideWithValue((_) => api),
          // **必须替掉**：不替的话这条测试会去读开发机上真实的
          // `%APPDATA%\cortex\settings.json`。里面那个地址一被读回来就会
          // 触发 `_reset()`，把这次续期判成「已被取代」——
          // 于是测试的成败取决于跑它的那台机器上存过什么地址
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
        ],
      );
      addTearDown(c.dispose);

      final ctrl = c.read(authControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await ctrl.signInWithPassword('u', 'MySecretPass123');
      expect(c.read(authControllerProvider).refreshToken, 'refresh-1');

      await ctrl.signOut();
      expect(api.logoutCalls, 1, reason: '只删本地副本不算登出');
      expect(api.lastRefreshSent, 'refresh-1');
      expect(c.read(authControllerProvider).refreshToken, isNull);
    });
  });
}
