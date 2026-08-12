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

  testWidgets('切到旧方式才出现 token 输入框', (tester) async {
    await _pumpLogin(tester, handler: (_) async => _json(_health));

    await tester.tap(find.text('用预共享 token 登录（旧方式）'));
    await tester.pumpAndSettle();

    expect(
      find.text('CORTEXD_TOKEN'),
      findsOneWidget,
      reason:
          '旧路必须还在：CLI、现有安装与单用户自托管都还在用它，'
          '一次性切断会让它们当天全部失联',
    );
    expect(find.text('用户名'), findsNothing);
  });
}

class _LiveConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: false, baseUrl: 'http://127.0.0.1:8080');
}
