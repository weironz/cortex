/// 离线模式：没有 cortexd 也能干活，但**必须一直说清楚没有记忆**。
///
/// # 这个模式的形状
///
/// 它与 mock 是两回事：mock 是假数据（为了看界面），离线模式里一切都是
/// 真的 —— 真模型、真工具、真读写本机文件，唯一缺的是记忆。
///
/// 下层早就支持了（`cortex-local` 断网时对话照常、写入排 outbox、
/// 明说「记忆未连接」）。缺的一直是桌面端那道门：它必须先探到一个可达的
/// cortexd 才过得了启动，探不到就停在登录界面。
///
/// 这几条盯着的，都是「改了之后不会报错、只会静默变糟」的地方。
library;

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('离线与 mock 互斥 —— 同时开着分不清眼前是真是假', () {
    final container = ProviderContainer();
    addTearDown(container.dispose);
    final cfg = container.read(appConfigProvider.notifier);

    cfg.setUseMock(true);
    cfg.setOffline(true);
    expect(
      container.read(appConfigProvider).useMock,
      isFalse,
      reason:
          '进离线模式必须把 mock 关掉。两个都开着的话，'
          '用户看到的是假数据却以为是自己的真文件',
    );
    expect(container.read(appConfigProvider).offline, isTrue);
  });

  test('点「离线使用」之后直接放行，不再探那个连不上的地址', () async {
    var probes = 0;
    final container = ProviderContainer(
      overrides: [
        authSeedTokenProvider.overrideWithValue(null),
        // 探测一律失败 —— 复现「服务器连不上」这个前提。
        // 在 health() 里抛而不是在工厂里抛：工厂抛出去是同步异常，
        // 会变成未捕获错误而不是「探测失败」这条业务路径
        authProbeApiProvider.overrideWithValue((token) {
          probes++;
          return _Unreachable();
        }),
      ],
    );
    addTearDown(container.dispose);

    // 真实顺序：先停在登录界面（探测失败），用户才点「离线使用」
    container.read(authControllerProvider);
    await Future<void>.delayed(Duration.zero);
    await Future<void>.delayed(Duration.zero);
    final before = probes;
    expect(before, greaterThan(0), reason: '前提没成立：一次都没探过');

    container.read(appConfigProvider.notifier).setOffline(true);
    await Future<void>.delayed(Duration.zero);
    await Future<void>.delayed(Duration.zero);

    expect(
      container.read(authControllerProvider).phase,
      AuthPhase.ready,
      reason:
          '点了离线使用还停在探测/不可达上，用户就进不去 —— '
          '而这个模式的全部意义就是「连不上也能用」',
    );
    expect(
      probes,
      before,
      reason:
          '进了离线模式还在探 —— 那个地址本来就不存在，'
          '每探一次就是让用户多等一个超时',
    );
  });

  test('AppConfig 带上 offline 之后，相等性与哈希都要认它', () {
    const a = AppConfig(useMock: false, baseUrl: 'http://x');
    const b = AppConfig(useMock: false, baseUrl: 'http://x', offline: true);
    expect(
      a == b,
      isFalse,
      reason:
          'offline 不进 == 的话，切换模式不会触发 riverpod 重建 —— '
          '表现是点了「离线使用」界面纹丝不动',
    );
    expect(a.hashCode == b.hashCode, isFalse);
  });

  test('copyWith 不会把 offline 悄悄抹掉', () {
    const on = AppConfig(useMock: false, baseUrl: 'http://x', offline: true);
    expect(
      on.copyWith(baseUrl: 'http://y').offline,
      isTrue,
      reason:
          '改地址把离线态顺手清了的话，用户会在毫无提示的情况下'
          '回到「等一个连不上的服务器」',
    );
  });
}

/// 一个连不上的 daemon：只有 `health()` 与真替身不同。
///
/// 派生自 mock 而不是从头实现 `CortexApi`：这个接口有几十个方法，
/// 手抄一份的下场是每加一个方法就要回来补一遍（本轮已经在别处
/// 补过五个测试替身了）。
class _Unreachable extends MockCortexApi {
  @override
  Future<HealthStatus> health() async => throw CortexApiException('连不上');
}
