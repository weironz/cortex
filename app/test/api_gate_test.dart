import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 非 mock 配置：让 `cortexApiProvider` 走真分支，而不是短路到 MockCortexApi。
class _LiveConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:9');
}

class _GateClosedAuth extends AuthController {
  @override
  AuthState build() => const AuthState(phase: AuthPhase.needsToken);
}

class _ProbingAuth extends AuthController {
  @override
  AuthState build() => const AuthState(phase: AuthPhase.probing);
}

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
