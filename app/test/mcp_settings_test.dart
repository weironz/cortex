import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/features/settings/pages/mcp_page.dart';
import 'package:cortex_app/models/mcp.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 一个答不了 MCP 的后端 —— Web 端与旧版本 agent 就是这样。
class _NoMcpApi extends MockCortexApi {
  @override
  Future<McpConfigView> mcpConfig() async =>
      throw const CortexApiException('这个后端没有本机 MCP。', statusCode: 404);
}

Widget _host(CortexApi api) => ProviderScope(
  overrides: [cortexApiProvider.overrideWithValue(api)],
  child: const MaterialApp(
    home: Scaffold(body: SizedBox(width: 900, height: 640, child: McpPage())),
  ),
);

void main() {
  Future<void> boot(WidgetTester tester, CortexApi api) async {
    tester.view.physicalSize = const Size(1200, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);
    await tester.pumpWidget(_host(api));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 200));
  }

  /// 连不上的那台**必须带着原因出现在列表里**。
  ///
  /// 这条是这一页存在的一半理由：少了几个工具在用户那儿的表现是
  /// 「模型今天有点笨」，而那是最难归因的一类故障。只列连上的那些，
  /// 等于把降级藏起来。
  testWidgets('连上的与连不上的都在列表里，且后者带着原因', (tester) async {
    await boot(tester, MockCortexApi());

    expect(find.text('filesystem'), findsOneWidget);
    expect(find.text('docs'), findsOneWidget);
    expect(
      find.textContaining('dns error'),
      findsOneWidget,
      reason: '连不上的原因要显示出来，否则用户只知道「少了点什么」',
    );
    expect(
      find.textContaining('已连接 · 2 个工具'),
      findsOneWidget,
      reason: '连上的那台要报出工具数',
    );
    expect(
      find.textContaining('1 台已连接 · 1 台异常'),
      findsOneWidget,
      reason: '顶部汇总要能一眼看出有东西坏了',
    );
  });

  /// 命令行**原样显示**。加一台 server = 在本机跑任意进程，这串东西是
  /// 用户能看到的唯一凭据。
  testWidgets('卡片上显示的是将要执行的那条命令', (tester) async {
    await boot(tester, MockCortexApi());
    expect(
      find.textContaining('npx -y @modelcontextprotocol/server-filesystem'),
      findsOneWidget,
    );
  });

  /// 没有本机 agent 时给的是**去哪配**，不是一个错误框。
  ///
  /// Web 端上这是常态。显示成错误的话，用户会以为自己该修点什么，
  /// 而实际上他要做的是往工作区放一个文件。
  testWidgets('答不了 MCP 的后端得到一句说明而不是红字', (tester) async {
    await boot(tester, _NoMcpApi());

    expect(find.text('MCP 需要本机 agent'), findsOneWidget);
    expect(
      find.textContaining('.mcp.json'),
      findsOneWidget,
      reason: '云端用户要在这里知道该往哪儿放配置，否则他会一直找一个不存在的开关',
    );
    expect(find.textContaining('重试'), findsNothing, reason: '这不是失败，不该给重试按钮');
  });

  /// 环境变量的**值**一个字都不能出现在界面上。
  testWidgets('详情页只显示环境变量的名字', (tester) async {
    await boot(tester, MockCortexApi());

    await tester.tap(find.text('docs'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 100));

    expect(find.text('AUTHORIZATION'), findsOneWidget);
    expect(
      find.textContaining('值不会回传到界面'),
      findsOneWidget,
      reason: '要说清为什么改它得重填一遍，否则那是个看起来像 bug 的空白',
    );
  });

  /// 默认档是「每次询问」，而且**说清为什么**。
  ///
  /// 这个控件的全部意义在那句解释上：MCP 的只读标记是服务端自报的，
  /// 认它等于把闸门的钥匙交给被闸的人。
  testWidgets('详情页的信任档位默认是每次询问且给出理由', (tester) async {
    await boot(tester, MockCortexApi());

    await tester.tap(find.text('filesystem'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 100));

    expect(find.text('每次询问'), findsWidgets);
    expect(find.textContaining('服务端自报'), findsOneWidget);
  });
}
