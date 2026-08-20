/// 「连得上」与「能跑云端对话」是两件事，连接页必须把它们分开说。
///
/// # 这一组盯着的那个假信号
///
/// 连接页原先只探 `GET /health`。它通了只说明这个地址上有个进程在答话 ——
/// 而一轮云端对话要走完另外三段：agent 编排服务向记忆服务换委托凭据、
/// 据它拉起沙箱容器、容器再回连编排服务。这三段断哪一段，`/health` 都照样
/// 200，于是这一页画绿灯「已连接」，用户回到对话框发一句，得到的是失败。
///
/// 生产上更狠：边缘按路径分流，`/health` 归**记忆服务**、`/sandbox/health`
/// 才是编排服务 —— 那条路上的 `/health` 连「编排服务在不在」都没有回答。
///
/// # 另一半：不可用**不许画成故障**
///
/// 自托管、纯本机、任何不接 docker 的部署本来就没有云沙箱。那是正常形态，
/// 把它渲染成红色错误，用户会去修一个没坏的东西 —— 而真正的失败
/// （编排服务在、但够不着记忆服务）反倒淹没在同一片红里。
@TestOn('vm')
library;

import 'dart:convert';

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/settings/pages/connection_page.dart';
import 'package:cortex_app/models/sandbox_health.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

/// 一份健康的 `/health` 回答 —— 这一组里它**永远是通的**，
/// 被测的正是「它通了之后还能不能看出云端对话跑不了」。
const _healthOk = {
  'status': 'ok',
  'version': '0.1.9',
  'database': 'ok',
  'auth': 'token',
};

/// agent 编排服务健康时的回答。字段名与形状取自真机实测（dev 环境
/// `curl http://127.0.0.1:5173/sandbox/health`）。
const _sandboxOk = {
  'status': 'ok',
  'version': '0.1.9',
  'role': 'agent-orchestrator',
  'callback_visible_to_sandbox': true,
  'database': 'ok',
  'auth': 'token',
};

/// 非 mock 配置：让 `cortexApiProvider` 走真客户端，而不是短路到 mock。
class _LiveConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:9');
}

/// 已登录 —— 否则 API 出口是 `GateClosedApi`，一个请求都发不出去。
class _ReadyAuth extends AuthController {
  @override
  AuthState build() => const AuthState(phase: AuthPhase.ready, token: 'tk');
}

/// 按路径分流的假后端：`/health` 与 `/sandbox/health` **各答各的**。
///
/// 刻意让它们不一致 —— 那正是生产上的实情（两条路由归两个进程），
/// 也是这一组测试唯一想制造的处境。
http.Client Function() _routing({
  required int sandboxStatus,
  required String sandboxBody,
  String sandboxContentType = 'application/json',
}) =>
    () => MockClient((request) async {
      if (request.url.path.endsWith('/sandbox/health')) {
        return http.Response(
          sandboxBody,
          sandboxStatus,
          headers: {'content-type': sandboxContentType},
        );
      }
      return http.Response(
        jsonEncode(_healthOk),
        200,
        headers: const {'content-type': 'application/json'},
      );
    });

HttpCortexApi _api(http.Client client) =>
    HttpCortexApi(baseUrl: 'http://127.0.0.1:9', token: 'tk', client: client);

void main() {
  group('探测本身', () {
    /// 每一种失败都是一个**要显示的答案**，不是一次异常。
    /// 抛出去的话，每个调用方都得自己判一次「这个错是不是其实正常」。
    test('404 = 这个部署没有沙箱，而且不抛', () async {
      final api = _api(MockClient((_) async => http.Response('', 404)));
      addTearDown(api.dispose);

      final probe = await api.sandboxHealth();
      expect(
        probe.status,
        CloudChatStatus.absent,
        reason:
            '404 是「这个地址上没有 agent 编排服务」。自托管与纯本机部署'
            '本来就是这样 —— 当成故障会让用户去修一个没坏的东西',
      );
    });

    /// CLAUDE.md 专门记过这个假信号：nginx 对认不出的路径做 SPA 回落，
    /// **回 200 + index.html，看起来像成功**。
    test('200 + 一张网页 = 没有沙箱，不是「探到了」', () async {
      final api = _api(
        MockClient(
          (_) async => http.Response(
            '<!DOCTYPE html><html><body>Cortex</body></html>',
            200,
            headers: {'content-type': 'text/html'},
          ),
        ),
      );
      addTearDown(api.dispose);

      expect(
        (await api.sandboxHealth()).status,
        CloudChatStatus.absent,
        reason:
            'SPA 回落的 200 必须被识破。认成 ready 的后果是这一页承诺一项'
            '这个部署根本给不了的能力，而用户要发一句话才发现',
      );
    });

    /// `role` 是「答话的到底是不是编排服务」唯一的判据：生产的边缘把
    /// `/health` 分给记忆服务，一份 200 的 JSON 也可能来自别人。
    test('答话的不是编排服务，按「没有沙箱」处理', () async {
      final api = _api(
        MockClient((_) async => http.Response(jsonEncode(_healthOk), 200)),
      );
      addTearDown(api.dispose);

      expect(
        (await api.sandboxHealth()).status,
        CloudChatStatus.absent,
        reason:
            '记忆服务的 /health 也是一份 200 的 JSON。不看 role 的话，'
            '一个根本没有 agentd 的部署会被判成「云端对话可用」',
      );
    });

    /// 502 与 404 **不能吞成同一个答案**：前者是「本该有，现在没起来」。
    test('网关 502 = 有沙箱但跑不起来，不是没有沙箱', () async {
      final api = _api(MockClient((_) async => http.Response('', 502)));
      addTearDown(api.dispose);

      final probe = await api.sandboxHealth();
      expect(
        probe.status,
        CloudChatStatus.blocked,
        reason:
            '并成 absent 的话，一次真正的宕机看起来会像一个正常的自托管形态 ——'
            '而这两种处境下用户该做的事完全相反',
      );
      expect(
        probe.reason,
        isNotNull,
        reason: 'blocked 必须带上原因，否则用户只知道「跑不了」，不知道下一步做什么',
      );
    });

    test('三段链路都通才算 ready', () async {
      final api = _api(
        MockClient((_) async => http.Response(jsonEncode(_sandboxOk), 200)),
      );
      addTearDown(api.dispose);

      final probe = await api.sandboxHealth();
      expect(probe.status, CloudChatStatus.ready);
      expect(
        probe.version,
        '0.1.9',
        reason:
            '编排服务的版本要单独报：它与 /health 那个可能来自两个进程，'
            '滚更新滚到一半时这是唯一看得出来的地方',
      );
    });

    /// 服务端自己去打过的那条。缺字段一律当通 —— 老版本不报它，
    /// 据此宣布「跑不了」会把一个能用的部署说成坏的。
    ///
    /// 这里以前还有一条 `memory_reachable=false`。记忆 2026-08-17 整个去掉了，
    /// 服务端不再报那个字段，客户端也不再读它。
    test('callback_visible_to_sandbox=false 点名断在哪一段', () async {
      final api = _api(
        MockClient(
          (_) async => http.Response(
            jsonEncode({..._sandboxOk, 'callback_visible_to_sandbox': false}),
            200,
          ),
        ),
      );
      addTearDown(api.dispose);

      final probe = await api.sandboxHealth();
      expect(probe.status, CloudChatStatus.blocked);
      expect(
        probe.reason,
        contains('回连'),
        reason: '这一段坏了的症状是「每一轮都在中途断掉」，与换不到凭据完全不同',
      );
    });

    test('老版本不报那两个字段时按通处理', () async {
      final api = _api(
        MockClient(
          (_) async => http.Response(
            jsonEncode(const {
              'role': 'agent-orchestrator',
              'version': '0.1.0',
            }),
            200,
          ),
        ),
      );
      addTearDown(api.dispose);

      expect(
        (await api.sandboxHealth()).status,
        CloudChatStatus.ready,
        reason:
            '缺字段当成「跑不了」，等于每次服务端加字段之前老客户端都在'
            '误报故障 —— 这个方向的误报比漏报贵得多',
      );
    });
  });

  group('连接页', () {
    Future<void> boot(
      WidgetTester tester, {
      required int sandboxStatus,
      required String sandboxBody,
      String sandboxContentType = 'application/json',
    }) async {
      tester.view.physicalSize = const Size(1200, 1400);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.reset);

      await tester.pumpWidget(
        ProviderScope(
          overrides: [
            appConfigProvider.overrideWith(_LiveConfig.new),
            authControllerProvider.overrideWith(_ReadyAuth.new),
            // 测试机上没有本地 agent 可拉起
            localAgentOriginProvider.overrideWith((ref) async => null),
            // 地址是存在磁盘上的，测试里不该去碰真磁盘
            settingsReaderProvider.overrideWithValue(() async => const {}),
            httpClientFactoryProvider.overrideWithValue(
              _routing(
                sandboxStatus: sandboxStatus,
                sandboxBody: sandboxBody,
                sandboxContentType: sandboxContentType,
              ),
            ),
          ],
          child: const MaterialApp(home: Scaffold(body: ConnectionPage())),
        ),
      );
      // 两条探测**先后**落地：`/sandbox/health` 是在第一帧里才发出去的，
      // 只 pump 一次的话它还停在 loading —— 那时断言什么都证明不了
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 200));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 200));
    }

    /// **设置里不许再出现 Mock 那个开关。**
    ///
    /// 它是开发用的实现细节被摆进了产品设置：Mock 是内存夹具、不连任何后端，
    /// 打开它等于把一个能用的客户端换成一个演示。问一句「用户关掉它能得到
    /// 什么好处」就露馅了 —— 没有，只会让人困惑「我现在的数据是真的吗」。
    ///
    /// 反向断言，与「输入框底部不许出现『沙箱』『容器』」同一个形状：
    /// 正向断言只会把下一个开发开关一起钉住。夹具本身留着（测试要用），
    /// 入口退回成构建参数 --dart-define=USE_MOCK=true。
    testWidgets('连接页不提供 Mock 开关', (tester) async {
      await boot(tester, sandboxStatus: 200, sandboxBody: '{}');

      for (final w in ['Mock', 'mock', '夹具']) {
        expect(
          find.textContaining(w),
          findsNothing,
          reason:
              '连接页出现了「$w」—— 内存夹具是开发用的东西，'
              '不该摆在用户的设置里让他去决定',
        );
      }
      expect(
        find.byType(SwitchListTile),
        findsNothing,
        reason: '连接页又多了一个开关 —— 加之前先问「用户关掉它能得到什么好处」',
      );
    });

    /// **这一条是整个改动的理由。**
    ///
    /// `/health` 通 + `/sandbox/health` 不通 = 一个能连上、但发不出云端
    /// 对话的部署。原先这一页只会显示 status/version/database 三行绿的，
    /// 而用户要回到对话框发一句话才知道跑不了。
    testWidgets('/health 通但 /sandbox/health 404：不报故障，但说明云端对话不可用', (
      tester,
    ) async {
      await boot(tester, sandboxStatus: 404, sandboxBody: '');

      // 找「云端对话：」而不是「云端对话」：地址输入框的提示语里也有这四个字
      // （「云端对话由它转给 agent 编排服务」），不带冒号会连它一起数进来 ——
      // 那样这条断言在功能被删掉之后照样绿
      expect(
        find.textContaining('云端对话：'),
        findsOneWidget,
        reason:
            '这一页必须自己说出「云端对话跑不了」。不说的话，'
            '三行绿的 /health 就是一个假信号 —— 用户要发一句话才发现',
      );
      expect(
        find.textContaining('没有沙箱'),
        findsOneWidget,
        reason: '要说清是**哪一种**不可用：这个部署压根没有沙箱，而不是它坏了',
      );

      // 「不是故障」这件事必须**可验证**，不能只靠人眼看颜色。
      // 判据：这一行的文字不许用 error 色 —— 那是留给真出事的。
      final scheme = Theme.of(
        tester.element(find.byType(ConnectionPage)),
      ).colorScheme;
      for (final text in tester.widgetList<Text>(
        find.textContaining('云端对话：'),
      )) {
        expect(
          text.style?.color,
          isNot(scheme.error),
          reason:
              '自托管与纯本机部署本来就没有沙箱 —— 画成红色错误，'
              '用户会去修一个没坏的东西',
        );
      }
    });

    /// 编排服务在、但它够不着记忆服务：**这一档才是真出事**，
    /// 而且必须与「没有沙箱」在界面上分得开。
    testWidgets('探到编排服务但链路断了：说出断在哪一段', (tester) async {
      await boot(
        tester,
        sandboxStatus: 200,
        sandboxBody: jsonEncode({
          ..._sandboxOk,
          'callback_visible_to_sandbox': false,
        }),
      );

      expect(
        find.textContaining('跑不起来'),
        findsOneWidget,
        reason: '这一档不是「没有」而是「本该能跑」—— 两句话不能长得一样',
      );
      expect(
        find.textContaining('回连不到'),
        findsOneWidget,
        reason:
            '断在哪一段决定用户下一步做什么（去看容器接没接上那张网）。'
            '只说「不可用」，他只能来问我们',
      );
      expect(
        find.textContaining('没有沙箱'),
        findsNothing,
        reason: '有沙箱、只是链路断了。说成「没有沙箱」会让人放弃排查',
      );
    });

    testWidgets('三段都通时说可用', (tester) async {
      await boot(
        tester,
        sandboxStatus: 200,
        sandboxBody: jsonEncode(_sandboxOk),
      );

      expect(find.textContaining('云端对话：可用'), findsOneWidget);
      // 带上「云端对话：」这个前缀，而不是光找「不可用」三个字：同一页上
      // 还有「本机 agent 没在跑 → 读写文件、跑命令不可用」，那是另一件事，
      // 而且在测试环境里它**本来就**没在跑。不限定前缀的话，这条断言会被
      // 一句正确的话弄红，然后有人跑去改那句正确的话
      expect(
        find.textContaining('云端对话：不可用'),
        findsNothing,
        reason: '通了还说不可用，用户会去关掉一个正常工作的部署',
      );
    });
  });
}
