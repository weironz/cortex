/// 登录界面**真的**在收账号密码，而不是收 token。
///
/// # 为什么这条测试值得单独存在
///
/// `AuthController.signInWithPassword` 上一版就写好了，也有测试覆盖 ——
/// 而登录界面里**没有任何一处调它**，屏幕上仍然只有一个 64 位十六进制的
/// 输入框。两边各自都是绿的，中间那根线没接上。
///
/// 单元测试测不到这个：控制器的测试直接调方法，界面的测试（当时不存在）
/// 才知道按钮按下去走的是哪一条。所以这里从**渲染出来的界面**出发，
/// 一路断言到发出去的那个请求。
library;

import 'dart:convert';

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/auth/login_gate.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

const _health = {
  'status': 'ok',
  'version': '0.1.5',
  'database': 'ok',
  'blob_backend': 's3',
  'auth': 'token',
};

http.Response _json(Object body, {int status = 200}) => http.Response(
  jsonEncode(body),
  status,
  headers: {'content-type': 'application/json; charset=utf-8'},
);

/// 立起登录界面，并记下它打出去的每一个请求。
Future<List<http.Request>> _pumpLogin(
  WidgetTester tester, {
  required Future<http.Response> Function(http.Request) handler,
}) async {
  final seen = <http.Request>[];
  final client = MockClient((req) {
    seen.add(req);
    return handler(req);
  });

  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        appConfigProvider.overrideWith(_LiveConfig.new),
        // 钉死种子：项目推荐的 `setx CORTEXD_TOKEN …` 会让"手上没有凭据"
        // 这一类用例在 CI 上过、在开发机上挂
        authSeedTokenProvider.overrideWithValue(null),
        authProbeApiProvider.overrideWithValue(
          (token) => HttpCortexApi(
            baseUrl: 'http://127.0.0.1:8080',
            token: token,
            client: client,
          ),
        ),
      ],
      child: const MaterialApp(home: LoginGate()),
    ),
  );
  // 控制器在 build 时会探一次；等它落地再断言，否则看到的是探测中的骨架
  await tester.pumpAndSettle();
  return seen;
}

void main() {
  testWidgets('默认收的是账号密码，不是 token', (tester) async {
    await _pumpLogin(tester, handler: (_) async => _json(_health));

    expect(
      find.widgetWithText(TextField, ''),
      findsWidgets,
      reason: '界面应该已经渲染出来了',
    );
    expect(
      find.text('用户名'),
      findsOneWidget,
      reason:
          '第一眼该看到的是用户名输入框。'
          '这条红了通常意味着界面又退回了"粘一串 64 位十六进制"那一版',
    );
    expect(find.text('密码'), findsOneWidget);
    expect(
      find.text('CORTEXD_TOKEN'),
      findsNothing,
      reason:
          'token 是旧路，不该出现在默认视图里 —— '
          '它应该藏在"用预共享 token 登录（旧方式）"后面',
    );
  });

  testWidgets('填账号密码点登录，打的是 /auth/login 且带着两个字段', (tester) async {
    final seen = await _pumpLogin(
      tester,
      handler: (req) async {
        if (req.url.path == '/auth/login') {
          return _json({
            'access_token': 'a' * 32,
            'refresh_token': 'r' * 32,
            'expires_in_secs': 900,
          });
        }
        return _json(_health);
      },
    );

    await tester.enterText(find.widgetWithText(TextField, '用户名'), '阿尔法');
    await tester.enterText(
      find.widgetWithText(TextField, '密码'),
      'hunter2-hunter2',
    );
    await tester.tap(find.widgetWithText(FilledButton, '登录'));
    await tester.pumpAndSettle();

    final login = seen.where((r) => r.url.path == '/auth/login').toList();
    expect(
      login,
      hasLength(1),
      reason:
          '按下"登录"必须真的打 /auth/login。'
          '一次都没打，说明按钮接的还是老的 signIn(token)，'
          '而那正是这次改动之前的样子 —— 界面看着换了，走的还是旧路',
    );
    final body = jsonDecode(login.single.body) as Map<String, dynamic>;
    expect(body['username'], '阿尔法');
    expect(body['password'], 'hunter2-hunter2');
    expect(
      login.single.url.query,
      isEmpty,
      reason: '凭据只能进请求体。进了 query string 就会落进 nginx 的访问日志',
    );
  });

  /// 登录页上**只有登录**：没有 token、没有 mock、没有那段凭据存储说明。
  ///
  /// 那三样各自有理由被拿掉：预共享 token 是「一台机器一把钥匙」的旧形态，
  /// 而现在有真的账号；mock 数据源是给「想看看界面长什么样」的人的，而它
  /// 出现在登录页上等于把一条演示路摆在正门；那段凭据存储说明讲的是 token
  /// 存哪儿的权衡，token 走了它也就没有主语了。
  ///
  /// 钉住的是**不出现**。一个被删掉的入口最容易的复活方式是「顺手加回来
  /// 方便调试」，而它不会有人反对 —— 直到它出现在生产的登录页上。
  testWidgets('登录页上只有登录，没有 token / mock / 存储说明', (tester) async {
    await _pumpLogin(tester, handler: (_) async => _json(_health));

    expect(find.text('用户名'), findsOneWidget);
    expect(find.text('密码'), findsOneWidget);

    for (final gone in ['用预共享 token 登录（旧方式）', 'CORTEXD_TOKEN', '用 Mock 数据源']) {
      expect(find.text(gone), findsNothing, reason: '「$gone」应当已经从登录页上拿掉了');
    }
    expect(
      find.textContaining('sessionStorage'),
      findsNothing,
      reason: '凭据存储那段说明是 token 那条路的注脚，主语没了它也该走',
    );
  });

  // ── 地址字段：默认收起 ────────────────────────────────────

  group('地址字段该不该出现', () {
    test('web 上永远不出现', () {
      for (final expanded in [true, false]) {
        for (final unreachable in [true, false]) {
          expect(
            shouldShowEndpointField(
              isWeb: true,
              expanded: expanded,
              unreachable: unreachable,
            ),
            isFalse,
            reason:
                'web 构建的 CORTEX_BASE_URL 是空串、走同源根路径 —— '
                '那个字段不是「可以不填」，是**填了就坏**：'
                '请求会从 nginx 同源那条路挪到一个没人接的绝对地址上。'
                '（expanded=$expanded, unreachable=$unreachable）',
          );
        }
      }
    });

    test('桌面上默认收起，点开才出现', () {
      expect(
        shouldShowEndpointField(
          isWeb: false,
          expanded: false,
          unreachable: false,
        ),
        isFalse,
        reason: '「部署入口地址」只有自托管的人答得上来，不该是第一屏第一个字段',
      );
      expect(
        shouldShowEndpointField(
          isWeb: false,
          expanded: true,
          unreachable: false,
        ),
        isTrue,
      );
    });

    test('连不上时自动展开', () {
      expect(
        shouldShowEndpointField(
          isWeb: false,
          expanded: false,
          unreachable: true,
        ),
        isTrue,
        reason:
            '编译期默认值是 127.0.0.1:8080，对任何有真部署的人都是错的。'
            '不自动展开的话，连不上的人看到的是一个没有出路的屏 —— '
            '而出路藏在他不知道要点的那行小字后面',
      );
    });
  });

  testWidgets('默认看不到地址输入框，只有一行小字；点了才出现', (tester) async {
    await _pumpLogin(tester, handler: (_) async => _json(_health));

    expect(
      find.text('部署入口地址'),
      findsNothing,
      reason: '每个新用户都要先处理一个与自己无关的字段 —— 这正是要拿掉的东西',
    );
    final link = find.widgetWithText(TextButton, '连接到你自己的部署');
    expect(link, findsOneWidget, reason: '收起来不等于拿走：自托管的人得有路可走');

    await tester.tap(link);
    await tester.pumpAndSettle();

    expect(find.text('部署入口地址'), findsOneWidget);
    expect(link, findsNothing, reason: '展开之后那行小字该让位，否则同一件事在屏幕上有两个入口');
  });

  testWidgets('离线卡片说清「看不到以前的会话」', (tester) async {
    await _pumpLogin(tester, handler: (_) async => _json(_health));

    expect(
      find.textContaining('也看不到以前的会话'),
      findsOneWidget,
      reason:
          'cortex-local 的 list_sessions 是纯转发，离线时列表只剩本机草稿。'
          '不说的话，用户看到的是「我昨天那些对话没了」—— '
          '而它们好好地在服务器上',
    );
    expect(
      find.textContaining('这段时间不会同步到服务端'),
      findsOneWidget,
      reason: '「不同步」与「看不到历史」是两件事，都要说',
    );
  });
}

class _LiveConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:8080');
}
