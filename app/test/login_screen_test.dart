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
    // **不能用 pumpAndSettle。** 按下之后按钮里是一个转圈的
    // `CircularProgressIndicator`，而这条用例只断言「请求发出去了」，
    // 不等整条登录流程走完 —— 于是 busy 一直是 true，动画一直在转，
    // pumpAndSettle 会一直等到超时。
    //
    // 换成静态文字能让它 settle，但那是**为了测试好写而把界面做差**：
    // 转圈比「登录中…」四个字更快看出有没有在动。
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

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

  testWidgets('离线卡片一行说清后果：不同步、看不到历史', (tester) async {
    await _pumpLogin(tester, handler: (_) async => _json(_health));

    // 2026-08-24 收敛：五行说明带两处粗体压过了主表单，详细后果搬去了
    // 进入离线之后的 `_OfflineBanner`（消息在它有用的那一刻出现）。
    // 登录页只留一行 —— 但两个要点必须都在：「不同步」防的是数据丢失的
    // 误解，「看不到历史」防的是「我昨天那些对话没了」的恐慌
    final line = find.textContaining('看不到');
    expect(line, findsOneWidget);
    expect(
      (tester.widget<Text>(line).data)!,
      contains('不同步'),
      reason: '「不同步」与「看不到历史」是两件事，一行里都要说到',
    );
    expect(
      find.textContaining('接上之后都回来'),
      findsOneWidget,
      reason: '只说损失不说恢复，用户会把「暂时看不到」读成「没了」',
    );
  });

  testWidgets('登录页不再承诺「记住 30 天」', (tester) async {
    await _pumpLogin(tester, handler: (_) async => _json(_health));

    expect(
      find.textContaining('30 天'),
      findsNothing,
      reason:
          '记住登录是行业默认预期，不值得占登录页一行；而只要还存在任何'
          '被误踢的路径，这句承诺就摆在「登录已过期」红字的正上方，读起来'
          '像嘲讽。正确的做法是修掉误踢（RefreshOutcome 那一批），不是辩解',
    );
  });

  // ── 用过的部署记得住、切得回 ──────────────────────────────

  group('记住用过的部署', () {
    test('最近用的在前，去重，封顶', () {
      var known = <String>[];
      known = AppConfig.remember(known, 'https://a/api');
      known = AppConfig.remember(known, 'http://127.0.0.1:5173');
      expect(known, ['http://127.0.0.1:5173', 'https://a/api']);

      known = AppConfig.remember(known, 'https://a/api');
      expect(known, [
        'https://a/api',
        'http://127.0.0.1:5173',
      ], reason: '再用一次要冒到最前面，而不是多出一条重复的');

      for (var i = 0; i < 20; i++) {
        known = AppConfig.remember(known, 'https://x$i/api');
      }
      expect(
        known.length,
        lessThanOrEqualTo(6),
        reason:
            '不封顶的话，几个月之后这份清单会长到没法用 —— '
            '而它的全部作用是「少打一次字」',
      );
    });

    test('尾斜杠不合并 —— 那可能真是两条路', () {
      final known = AppConfig.remember(
        AppConfig.remember([], 'https://a/api'),
        'https://a/api/',
      );
      expect(known.length, 2, reason: '替用户做 URL 归一化等于替他改配置，而服务端那侧它们可能真不一样');
    });
  });

  testWidgets('展开地址后能收起，也能一点切回用过的部署', (tester) async {
    await _pumpLogin(tester, handler: (_) async => _json(_health));
    await tester.tap(find.widgetWithText(TextButton, '连接到你自己的部署'));
    await tester.pumpAndSettle();

    // 用过的那个（不是当前这个）该摆成可点的
    final chip = find.widgetWithText(ActionChip, 'cortex.example.com');
    expect(
      chip,
      findsOneWidget,
      reason:
          '没有它，「切回另一个部署」就等于凭记忆重打一串 URL —— '
          '而人记不住自己的部署地址是完全正常的事',
    );
    await tester.tap(chip);
    await tester.pumpAndSettle();
    expect(
      find.text('https://cortex.example.com/api'),
      findsOneWidget,
      reason: '点了就该把地址填进去',
    );

    await tester.tap(find.byTooltip('收起'));
    await tester.pumpAndSettle();
    expect(
      find.text('部署入口地址'),
      findsNothing,
      reason:
          '展开之后没有回头路的话，一个连不上、想换回另一个部署的人'
          '在这一屏上就没有任何可点的东西了',
    );
  });

  group('注册入口', () {
    testWidgets('部署没说开注册：登录页上没有注册入口', (tester) async {
      // `_health` 里没有 open_registration，/sandbox/health 的补问也拿到
      // 同一份（role 对不上 → absent → null）—— 两条路都答不出就是关
      await _pumpLogin(tester, handler: (_) async => _json(_health));

      expect(
        find.text('注册新账号'),
        findsNothing,
        reason:
            '部署没开注册时不许摆入口 —— 摆一个必然 403 的入口，'
            '与提示词里写没接的能力是同一个错（约束 2）',
      );
    });

    testWidgets('部署开着注册：入口出现，切过去提交打的是 /auth/register', (tester) async {
      final seen = await _pumpLogin(
        tester,
        handler: (req) async {
          if (req.url.path == '/auth/register') {
            return _json({
              'access_token': 'a' * 32,
              'refresh_token': 'r' * 32,
              'access_expires_in_secs': 900,
            });
          }
          return _json({..._health, 'open_registration': true});
        },
      );

      final entry = find.text('注册新账号');
      expect(entry, findsOneWidget, reason: '服务端说开了，入口就得在');

      await tester.tap(entry);
      await tester.pumpAndSettle();
      expect(find.text('确认密码'), findsOneWidget, reason: '注册表单要求确认一遍密码');

      await tester.enterText(find.widgetWithText(TextField, '用户名'), 'newbie');
      await tester.enterText(
        find.widgetWithText(TextField, '密码'),
        'hunter2-hunter2',
      );
      await tester.enterText(
        find.widgetWithText(TextField, '确认密码'),
        'hunter2-hunter2',
      );
      await tester.tap(find.widgetWithText(FilledButton, '注册并登录'));
      // 与登录那条同一个理由不用 pumpAndSettle：按钮在转圈
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 50));

      final register = seen
          .where((r) => r.url.path == '/auth/register')
          .toList();
      expect(
        register,
        hasLength(1),
        reason: '按下「注册并登录」必须真的打 /auth/register —— 界面与控制器中间那根线要接上',
      );
      final body = jsonDecode(register.single.body) as Map<String, dynamic>;
      expect(body['username'], 'newbie');
      expect(body['password'], 'hunter2-hunter2');
    });

    testWidgets('两次密码不一致：本地拦下，一个请求都不发', (tester) async {
      final seen = await _pumpLogin(
        tester,
        handler: (_) async => _json({..._health, 'open_registration': true}),
      );

      await tester.tap(find.text('注册新账号'));
      await tester.pumpAndSettle();
      await tester.enterText(find.widgetWithText(TextField, '用户名'), 'newbie');
      await tester.enterText(
        find.widgetWithText(TextField, '密码'),
        'hunter2-hunter2',
      );
      await tester.enterText(
        find.widgetWithText(TextField, '确认密码'),
        'hunter2-hunter3',
      );
      await tester.tap(find.widgetWithText(FilledButton, '注册并登录'));
      await tester.pumpAndSettle();

      expect(find.text('两次输入的密码不一致。'), findsOneWidget);
      expect(
        seen.where((r) => r.url.path == '/auth/register'),
        isEmpty,
        reason: '打错字是唯一一条客户端自己判的 —— 不该为它烧一次注册限流额度',
      );
    });
  });
}

class _LiveConfig extends AppConfigNotifier {
  @override
  AppConfig build() => const AppConfig(
    useMock: false,
    baseUrl: 'http://127.0.0.1:8080',
    // 装一个「以前用过、现在没在用」的部署 —— 切回去那条路正是要测的
    knownBaseUrls: ['http://127.0.0.1:8080', 'https://cortex.example.com/api'],
  );
}
