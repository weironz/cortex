/// 登录页必须**在你动手之前**说清连的是哪个环境。
///
/// # 案发现场（2026-08-29）
///
/// 用户的原话：「关键是这个页面没有告诉我我连的是哪个环境啊，我没有点连
/// 自己的服务器，默认就应该永远连官方的服务器地址才对啊」。
///
/// 当时的链条是两截：
///
/// 1. 地址被之前调试 dev 时存下来了（`127.0.0.1:5173`），而登录页从头到尾
///    一个字都没提 —— 直到点了登录，才从一行 Dart 的 `SocketException`
///    里露出来（还带着一个毫无意义的临时端口号）。
/// 2. 更靠前的一截：`release-desktop-windows.sh` **没传
///    `--dart-define=CORTEX_BASE_URL`**，于是发出去的安装包默认连
///    `127.0.0.1:8080`。那一截由那个脚本里的断言守着（漏传就打不出包）。
///
/// 这一组守第一截：**分类要说实话，而且那句话要常驻。**
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/auth/login_gate.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 把认证状态直接构造成「要登录」。
///
/// 不这么做的话 `LoginGate` 会停在「正在检查登录状态」那一段（真实的启动
/// 流程要读平台凭据库，测试里读不到），一个 Text 都不渲染 —— 于是断言
/// 全都落空，而那种绿是假的。
class _Gate extends AuthController {
  @override
  AuthState build() => const AuthState(phase: AuthPhase.needsToken);
}

class _Cfg extends AppConfigNotifier {
  _Cfg(this._url);
  final String _url;
  @override
  AppConfig build() => AppConfig(baseUrl: _url, useMock: false);
}

void main() {
  group('这个地址属于哪一类', () {
    const official = 'https://cortex.cloudcele.com/api';

    test('回环地址一律是「本机」，不管端口是几', () {
      for (final u in [
        'http://127.0.0.1:5173',
        'http://127.0.0.1:8080',
        'http://localhost:3000',
        'https://127.0.0.1',
      ]) {
        expect(
          classifyEndpoint(u, officialUrl: official),
          EndpointKind.loopback,
          reason:
              '$u 是本机地址。判成官方的话，那一行就在骗人 —— '
              '而那正是用户撞上的：界面什么都没说，他以为自己连的是官方',
        );
      }
    });

    test('同一个 host 就是官方 —— 路径与端口不参与判断', () {
      for (final u in [
        official,
        'https://cortex.cloudcele.com',
        'https://cortex.cloudcele.com/api/',
      ]) {
        expect(
          classifyEndpoint(u, officialUrl: official),
          EndpointKind.official,
          reason:
              '$u 与官方是同一台服务器。按整串比的话，'
              '一个末尾多写了斜杠的地址会被标成「你自己的部署」—— 那是虚惊',
        );
      }
    });

    test('别处就是「你自己的部署」', () {
      for (final u in [
        'https://cortex.example.com/api',
        'https://192.168.1.9:8080',
      ]) {
        expect(u, isNotEmpty);
        expect(classifyEndpoint(u, officialUrl: official), EndpointKind.custom);
      }
    });

    test('残缺的地址不当成官方', () {
      // 宁可标成「自己的部署」也不能标成官方：前者让人多看一眼，
      // 后者让人放心地连到一个说不清是哪儿的地方
      for (final u in ['', '不是地址', 'https://']) {
        expect(
          classifyEndpoint(u, officialUrl: official),
          EndpointKind.custom,
          reason: '$u 判不出来 —— 判不出来时不许说「官方」',
        );
      }
    });

    test('官方地址本身是空串时，谁都不是官方', () {
      // Web 构建把 CORTEX_BASE_URL 传成空串（同源根路径）。那份构建里
      // 这一行本来就不画，但分类函数不能因此把任意地址认成官方
      expect(
        classifyEndpoint('https://cortex.cloudcele.com', officialUrl: ''),
        EndpointKind.custom,
        reason: '没有官方地址可比时，不能凭空认亲',
      );
    });
  });

  group('那一行必须常驻 —— 不是点了登录才说', () {
    testWidgets('连着本机 dev 时，登录页上直接看得见', (tester) async {
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(() => _Cfg('http://127.0.0.1:5173')),
          authControllerProvider.overrideWith(_Gate.new),
          cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        ],
      );
      addTearDown(c.dispose);
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: const MaterialApp(home: LoginGate()),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 50));

      // **一次点击都没有** —— 这正是用户要的：动手之前就知道
      expect(
        find.textContaining('本机'),
        findsWidgets,
        reason:
            '连着本机 dev 而界面不说，用户会以为自己连的是官方 —— '
            '2026-08-29 就是这么撞上的',
      );
      expect(
        find.textContaining('127.0.0.1:5173'),
        findsWidgets,
        reason: '要把地址本身写出来：只说「本机」，用户仍然不知道是哪一份',
      );
      // ⚠️ 这里断言的是**没有**这个按钮，而且那是对的：
      //
      // 测试构建没有 `--dart-define=CORTEX_BASE_URL`，于是
      // `AppConfig.defaultBaseUrl` 是 `127.0.0.1:8080` —— 一个回环地址。
      // 那种构建里「官方」根本不存在，所以摆不出「换回官方」。
      //
      // 摆一个点了没地方去的按钮，比没有这个按钮糟得多（这个仓库反复
      // 记过这个形状）。真正的产物里 `defaultBaseUrl` 是官方地址，
      // 那时这个按钮才出现 —— 而「产物里必须有官方地址」由
      // `release-desktop-windows.sh` 的断言守着，不由这条测试守。
      expect(
        find.widgetWithText(TextButton, '换回官方服务器'),
        findsNothing,
        reason: '没有官方地址可去时，不许摆一个点了没反应的按钮',
      );

      await tester.pumpWidget(const SizedBox.shrink());
      await tester.pump();
      await tester.pump();
    });
  });
}
