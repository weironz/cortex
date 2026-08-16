import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

/// 非 mock 配置：让 `cortexApiProvider` 走真分支，而不是短路到 MockCortexApi。
class _LiveConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:9');
}

class _OfflineConfig extends AppConfigNotifier {
  @override
  AppConfig build() => const AppConfig(
    useMock: false,
    baseUrl: 'http://127.0.0.1:9',
    offline: true,
  );
}

class _GateClosedAuth extends AuthController {
  @override
  AuthState build() => const AuthState(phase: AuthPhase.needsToken);
}

class _ProbingAuth extends AuthController {
  @override
  AuthState build() => const AuthState(phase: AuthPhase.probing);
}

/// 可翻相位、可记 401 报告次数的替身。
///
/// 存在的理由：审查用**变异测试**证明过，把 provider 里的 `watch` 改成
/// `read`（登录后全 App 抱着桩不放）时，固定相位的替身全绿 ——
/// 只有真的翻一次相位、看 provider 是否换代，才护得住那行 watch。
class _MutableAuth extends AuthController {
  _MutableAuth(this._initial);
  final AuthState _initial;
  int unauthorizedCalls = 0;

  @override
  AuthState build() => _initial;

  void go(AuthState next) => state = next;

  @override
  Future<void> onUnauthorized() async {
    unauthorizedCalls++;
  }
}

/// 每个请求都答 401 的 HTTP 层 —— 从传输层触发 `onUnauthorized` 闭包，
/// 而不是绕过它直接调 notifier。
http.Client Function() _always401() =>
    () => MockClient((_) async => http.Response('{"error":"nope"}', 401));

void main() {
  ProviderContainer boot(AuthController Function() auth) {
    final c = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_LiveConfig.new),
        authControllerProvider.overrideWith(auth),
      ],
    );
    addTearDown(c.dispose);
    return c;
  }

  group('登录门没开时的 API 出口', () {
    /// 这条测试钉的是 2026-08 那次「登录不上」的根因之一：登录页阶段
    /// 各个 controller 照常开火，打出一串 401 —— 每个 401 都可能敲
    /// `onUnauthorized`，把红字压在还没输过密码的登录表单上。
    /// 门装在唯一的出口（`cortexApiProvider`）上，漏不掉新增的消费方。
    test('needsToken 时拿到的是桩，任何调用不碰网络直接抛', () {
      final api = boot(_GateClosedAuth.new).read(cortexApiProvider);

      expect(api, isA<GateClosedApi>(), reason: '门关着还发真客户端，登录页就又会是一串 401');
      expect(
        () => api.sessions(),
        throwsA(
          isA<CortexApiException>().having(
            (e) => e.message,
            'message',
            contains('gate-closed'),
          ),
        ),
        reason:
            '桩必须抛而不是回空列表 —— 空列表会把「还没登录」'
            '伪装成「没有会话」',
      );
    });

    test('probing 时同样关门 —— 还不知道要不要凭据，发出去是在赌', () {
      expect(
        boot(_ProbingAuth.new).read(cortexApiProvider),
        isA<GateClosedApi>(),
      );
    });

    test('桩的 dispose 不抛 —— provider 换代时会真的调它', () {
      expect(() => GateClosedApi().dispose(), returnsNormally);
    });

    /// Stream 成员不许同步抛。真实现全是 async*（永不同步抛），调用方
    /// 据此把「先置 running、再 listen」写成了直筒代码 —— 同步抛会在
    /// `.listen` 之前穿出去：状态卡死、异常冲进 unhandled zone。
    test('Stream 成员抛在流里，不抛在调用点', () {
      final api = GateClosedApi();
      expect(() => api.watchSync(), returnsNormally);
      expectLater(
        api.watchSync(),
        emitsError(isA<CortexApiException>()),
        reason: '错误要从消费方现成的 onError 路径走',
      );
      expect(
        () => api.chat(sessionId: 's', message: 'hi'),
        returnsNormally,
        reason: '会话过期时开着的界面点「发送」不该炸 unhandled zone',
      );
    });

    /// 变异测试确认过的回归形态：把门那行 `watch` 改成 `read`，登录成功后
    /// 整个 App 继续抱着桩，每个请求都抛 gate-closed —— 而固定相位的测试
    /// 全绿。这条测试真的翻相位，两个方向都盯。
    test('登录后桩换成真客户端，登出后换回来', () async {
      final auth = _MutableAuth(const AuthState(phase: AuthPhase.needsToken));
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_LiveConfig.new),
          authControllerProvider.overrideWith(() => auth),
          // 别让 ready 分支真的去探测/拉起本地 agent —— 测试机上没有
          localAgentOriginProvider.overrideWith((ref) async => null),
        ],
      );
      addTearDown(c.dispose);

      expect(c.read(cortexApiProvider), isA<GateClosedApi>());

      auth.go(const AuthState(phase: AuthPhase.ready, token: 'tk-a'));
      expect(
        c.read(cortexApiProvider),
        isA<HttpCortexApi>(),
        reason: '登录成功 provider 必须换代 —— 抱着桩不放等于登录无效',
      );

      auth.go(const AuthState(phase: AuthPhase.needsToken));
      expect(
        c.read(cortexApiProvider),
        isA<GateClosedApi>(),
        reason: '登出要把门关回去，否则退出登录后请求还在往外发',
      );
    });
  });

  group('onUnauthorized 闭包的接线（走真传输层）', () {
    /// 上一轮只有纯函数测试，闭包本体一次都没跑过 —— 接线错了
    /// （比如两边都读现值，判据退化成 x==x）照样全绿。这组测试从
    /// MockClient 的 401 应答一路打到 notifier，护的就是接线本身。
    ProviderContainer bootReady(_MutableAuth auth, {bool offline = false}) {
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(
            offline ? _OfflineConfig.new : _LiveConfig.new,
          ),
          authControllerProvider.overrideWith(() => auth),
          localAgentOriginProvider.overrideWith((ref) async => null),
          httpClientFactoryProvider.overrideWithValue(_always401()),
        ],
      );
      addTearDown(c.dispose);
      return c;
    }

    Future<void> fire(CortexApi api) async {
      try {
        await api.sessions();
      } on CortexApiException {
        // 401 本身是预期的；要看的是闭包接下来做什么
      }
      // onUnauthorized 是 scheduleMicrotask 出来的，让它落地
      await Future<void>.delayed(Duration.zero);
    }

    test('现任实例的 401 → 转发给 AuthController', () async {
      final auth = _MutableAuth(
        const AuthState(phase: AuthPhase.ready, token: 'tk-a'),
      );
      final c = bootReady(auth);

      await fire(c.read(cortexApiProvider));

      expect(
        auth.unauthorizedCalls,
        1,
        reason: '服务端重启清了 access 簿子时，全靠这条转发去触发续期',
      );
    });

    test('退位实例的迟到 401 → 丢弃', () async {
      final auth = _MutableAuth(
        const AuthState(phase: AuthPhase.ready, token: 'tk-a'),
      );
      final c = bootReady(auth);
      final oldApi = c.read(cortexApiProvider);

      // 续期轮换：token 换代，provider 重建 —— oldApi 成了上一代
      auth.go(const AuthState(phase: AuthPhase.ready, token: 'tk-b'));
      expect(c.read(cortexApiProvider), isNot(same(oldApi)));

      await fire(oldApi);

      expect(
        auth.unauthorizedCalls,
        0,
        reason:
            '迟到的 401 一旦放进去，就是「登录成功 → 续期 → 熔断 → '
            '被打回登录页」那条死循环的第一步',
      );
    });

    test('离线模式：远端打来的 401 一概不转发', () async {
      final auth = _MutableAuth(
        // 离线模式的形态：ready 且没有任何凭据
        const AuthState(phase: AuthPhase.ready),
      );
      final c = bootReady(auth, offline: true);

      await fire(c.read(cortexApiProvider));

      expect(
        auth.unauthorizedCalls,
        0,
        reason:
            '主动选离线的用户手里本来就没有凭据 —— 远端 401 不构成'
            '「你被登出了」。放进去的实测后果：点「离线使用」→ 被打回'
            '登录页，且按钮从此同值短路、点不动',
      );
    });
  });

  group('401 报告的资格审查', () {
    /// 钉的是另一半根因：token 一换实例就换代，而 401 经微任务上报 ——
    /// 「上一代实例发的请求」的 401 会在新凭据生效**之后**才落地。
    /// 放它进去的实测后果：登录成功 → 迟到的无凭据 401 触发续期（成功）
    /// → 3 秒内又一条迟到 401 → 熔断器把刚登录的人打回登录页。
    test('实例的 token 不是现任 —— 迟到的报告，丢掉', () {
      expect(
        shouldForwardUnauthorized(instanceToken: null, currentToken: 'now'),
        isFalse,
        reason:
            '登录前发出的无凭据请求，其 401 在登录后落地 —— 正是打回'
            '登录页那条死循环的第一步',
      );
      expect(
        shouldForwardUnauthorized(instanceToken: 'old', currentToken: 'now'),
        isFalse,
        reason: '续期换代后，旧 access token 实例的迟到 401 与现任无关',
      );
    });

    test('实例的 token 就是现任 —— 真的被拒了，放行', () {
      expect(
        shouldForwardUnauthorized(instanceToken: 'now', currentToken: 'now'),
        isTrue,
        reason: '服务端重启清了 access 簿子时，就靠这条放行去触发续期',
      );
      expect(
        shouldForwardUnauthorized(instanceToken: null, currentToken: null),
        isTrue,
        reason: '关认证的部署中途把认证打开：这是唯一能把用户送回登录页的路',
      );
    });
  });
}
