/// 精选连接器那一条。
///
/// 盯住的四件事：
///
/// 1. **已经接过的那条能不能再点** —— `PUT` 是覆盖，再点一次会把用户改过的
///    trust、填过的 key 一起盖掉，而界面上什么都不会发生
/// 2. **接出来的是不是一台普通的 MCP server** —— 名字与配置要落到同一条
///    `saveMcpServer` 上，不能另起一套
/// 3. **要 key 的那条有没有提前说** —— 点下去才发现要一把令牌是最糟的
/// 4. **精选里有没有混进不该有的东西** —— 本地文件（绕过工作区围栏）
///    与记忆（这个仓库根本没有那个能力）两条是有意不收的
library;

import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/features/settings/pages/connector_presets.dart';
import 'package:cortex_app/features/settings/pages/mcp_page.dart';
import 'package:cortex_app/models/mcp.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 记下「接入」到底往服务端写了什么。
class _SpyApi extends MockCortexApi {
  String? savedName;
  Map<String, dynamic>? savedConfig;

  @override
  Future<McpConfigView> saveMcpServer({
    required String name,
    required Map<String, dynamic> config,
    String trust = 'ask',
    bool disabled = false,
    List<String> removeEnv = const [],
  }) async {
    savedName = name;
    savedConfig = config;
    return super.saveMcpServer(
      name: name,
      config: config,
      trust: trust,
      disabled: disabled,
      removeEnv: removeEnv,
    );
  }
}

Widget _host(CortexApi api) => ProviderScope(
  overrides: [cortexApiProvider.overrideWithValue(api)],
  child: const MaterialApp(
    home: Scaffold(body: SizedBox(width: 1000, height: 700, child: McpPage())),
  ),
);

Future<void> _boot(WidgetTester tester, CortexApi api) async {
  tester.view.physicalSize = const Size(1400, 1000);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(_host(api));
  for (var i = 0; i < 12; i++) {
    await tester.pump(const Duration(milliseconds: 20));
  }
}

void main() {
  test('精选里没有本地文件，也没有记忆', () {
    final ids = connectorPresets.map((p) => p.id).toList();
    expect(
      ids.any((id) => id.contains('filesystem') || id.contains('file')),
      isFalse,
      // 我们已经有文件工具，而且它们受工作区那道围栏管。再接一台能读整块盘
      // 的，等于在围栏边上开一扇没锁的门，而用户看不出这两套的区别
      reason: '本地文件那条是有意不收的：它绕过工作区围栏，而界面上看不出区别',
    );
    expect(
      ids.any((id) => id.contains('memory')),
      isFalse,
      // 这个仓库 2026-08-17 把长期记忆整条拆走了。摆一个叫「记忆」的连接器
      // 在这儿，只会让人以为那件事回来了
      reason: '这个仓库没有长期记忆。摆一个叫「记忆」的连接器等于替一件不存在的事打广告',
    );
  });

  test('每条精选都是 npx -y，且带说明', () {
    for (final p in connectorPresets) {
      final args = (p.config['args'] as List).cast<String>();
      expect(
        args.first,
        '-y',
        // 漏了 -y 的话，npx 会问「要装 xxx 吗 (y/N)」，而那句问话在 stdio 上
        // 没人回答得了 —— 表现是这台 server 永远连不上，日志里什么都看不出来
        reason: '${p.id} 漏了 -y：npx 会停在一句没人回答得了的问话上，而它看起来只是「连不上」',
      );
      expect(
        p.description.trim(),
        isNotEmpty,
        reason: '${p.id} 没写说明 —— 用户没法判断该不该接它',
      );
      expect(
        p.name.trim(),
        isNotEmpty,
        reason: '${p.id} 没有 server 名，而那个名字会变成工具名的前缀',
      );
    }
  });

  testWidgets('点一条精选，落到的是同一条 saveMcpServer', (tester) async {
    final api = _SpyApi();
    await _boot(tester, api);

    final pdf = connectorPresets.firstWhere((p) => p.id == 'pdf');
    await tester.ensureVisible(find.byKey(const ValueKey('connector:pdf')));
    await tester.tap(find.byKey(const ValueKey('connector:pdf')));
    for (var i = 0; i < 12; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }

    expect(
      api.savedName,
      pdf.name,
      // 另起一套的话，这台 server 不会出现在列表里、删不掉、也改不了 trust
      reason: '精选接出来的必须是一台普通的 MCP server，走同一条落盘路径',
    );
    expect(api.savedConfig?['command'], 'npx');
  });

  testWidgets('已经接过的那条不给再点 —— PUT 是覆盖', (tester) async {
    final api = _SpyApi();
    await _boot(tester, api);

    // mock 后端预置了一台叫 `docs` 的 —— 与「库文档」那条精选同名
    final docs = connectorPresets.firstWhere((p) => p.name == 'docs');
    final chip = find.byKey(ValueKey('connector:${docs.id}'));
    await tester.ensureVisible(chip);
    await tester.tap(chip);
    for (var i = 0; i < 12; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }

    expect(
      api.savedName,
      isNull,
      // 再点一次会把用户改过的 trust、填过的 key 一起盖掉，
      // 而界面上什么都不会发生 —— 这是最难发现的一类「操作生效了但是错的」
      reason: '已经接过的那条还能点的话，一次误触就把用户配好的东西盖掉了，且毫无提示',
    );
    expect(
      find.byIcon(Icons.check_rounded),
      findsWidgets,
      reason: '不给点就得看得出为什么 —— 一个点了没反应的按钮读起来像坏了',
    );
  });

  testWidgets('要 key 的那条提前挂一把钥匙', (tester) async {
    await _boot(tester, _SpyApi());

    final github = connectorPresets.firstWhere((p) => p.needsKey);
    expect(github.envHint!.where.trim(), isNotEmpty, reason: '得说清去哪儿拿这把 key');

    await tester.ensureVisible(find.byKey(ValueKey('connector:${github.id}')));
    expect(
      find.byIcon(Icons.key_outlined),
      findsWidgets,
      // 点下去才发现要一把令牌，是这一排上最糟的体验
      reason: '要填 key 的那条得提前标出来',
    );
  });
}
