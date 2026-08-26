/// 终端面板的簿子：标签、关掉哪一个、收起、铺满、换会话。
///
/// # 为什么这几件事各自值得一条
///
/// 它们全都**不报错地错**：
///
/// 1. 「关掉这一个」与「收起整栏」共用一个 × 的话，用户会以为点了收起
///    就把 `npm run dev` 停了（或者反过来，以为没停）。两者在这一层是
///    两个方法，测试盯住它们没有合流。
/// 2. 关掉当前标签之后接住谁 —— 接错了的表现是「连着关几个，每关一次
///    跳一次」，读起来像界面在抽搐。
/// 3. 标签的 id 必须稳定：拿序号当 key 的话，关掉中间那个之后后面的标签
///    集体前移，于是它们各自换了一个 shell，而屏幕上看起来只是少了一个。
/// 4. 换会话必须清空：每个 shell 的 cwd 来自那条会话的工作区绑定，
///    留着就是一排站在错目录里的 shell。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/terminal_panel.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

ProviderContainer _boot() {
  final c = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_MockConfig.new),
      cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
      settingsReaderProvider.overrideWithValue(
        () async => const <String, String>{},
      ),
      settingsWriterProvider.overrideWithValue((_) async {}),
    ],
  );
  // provider 是懒的：不订阅的话 build() 里那个「换会话就清空」的
  // listen 根本没装上
  c.listen(terminalPanelProvider, (_, _) {}, fireImmediately: true);
  return c;
}

void main() {
  group('标签', () {
    test('新建的标签排在后面并被选中，序号只增不补', () {
      final c = _boot();
      addTearDown(c.dispose);
      final p = c.read(terminalPanelProvider.notifier);

      p.addTab();
      p.addTab();
      var st = c.read(terminalPanelProvider);
      expect(st.tabs.map((t) => t.label), ['终端 1', '终端 2']);
      expect(st.activeId, st.tabs.last.id, reason: '新建的要跳过去 —— 不然点了加号看不出发生了什么');

      p.close(st.tabs.first.id);
      p.addTab();
      st = c.read(terminalPanelProvider);
      expect(
        st.tabs.map((t) => t.label),
        ['终端 2', '终端 3'],
        reason:
            '补空位（新建的还叫 1）在一个正跑着东西的终端旁边会读错人 —— '
            '「刚才那个终端 1」和「现在这个终端 1」是两个 shell',
      );
    });

    test('每个标签的 id 各不相同', () {
      final c = _boot();
      addTearDown(c.dispose);
      final p = c.read(terminalPanelProvider.notifier);
      p.addTab();
      p.addTab();
      p.addTab();

      final ids = c.read(terminalPanelProvider).tabs.map((t) => t.id).toSet();
      expect(
        ids.length,
        3,
        reason:
            'id 是部件的 key。撞了或者用序号的话，关掉中间那个之后后面的'
            '标签会集体前移，于是它们各自换了一个 shell，而屏幕上看起来'
            '只是少了一个',
      );
    });

    test('关掉当前那个，接住它右边的；它是最后一个就接左边', () {
      final c = _boot();
      addTearDown(c.dispose);
      final p = c.read(terminalPanelProvider.notifier);
      p.addTab();
      p.addTab();
      p.addTab();
      final ids = c.read(terminalPanelProvider).tabs.map((t) => t.id).toList();

      p.select(ids[1]);
      p.close(ids[1]);
      expect(
        c.read(terminalPanelProvider).activeId,
        ids[2],
        reason: '接住右边那个 —— 与浏览器标签页一致。总是回到第一个的话，连着关几个会每关一次跳一次',
      );

      p.close(ids[2]);
      expect(
        c.read(terminalPanelProvider).activeId,
        ids[0],
        reason: '最后一个被关掉时接左边',
      );
    });

    test('关掉的不是当前那个，当前不变', () {
      final c = _boot();
      addTearDown(c.dispose);
      final p = c.read(terminalPanelProvider.notifier);
      p.addTab();
      p.addTab();
      final ids = c.read(terminalPanelProvider).tabs.map((t) => t.id).toList();
      p.select(ids[1]);

      p.close(ids[0]);
      expect(
        c.read(terminalPanelProvider).activeId,
        ids[1],
        reason: '关掉别的标签把当前这个也换掉了 —— 用户正看着的输出会被顶走',
      );
    });
  });

  group('两个 × 是两件事', () {
    /// ⚠️ 这一条是这一组的核心：面板右上角那个 × **不许动任何 shell**。
    ///
    /// 它们合流的表现极难查：用户以为「收起」只是不看了，回头发现
    /// `npm run dev` 早就停了；或者反过来，以为关掉了却还在后台跑着。
    test('收起整栏不关掉任何 shell', () {
      final c = _boot();
      addTearDown(c.dispose);
      final p = c.read(terminalPanelProvider.notifier);
      p.addTab();
      p.addTab();

      p.hide();

      expect(
        c.read(terminalPanelProvider).tabs.length,
        2,
        reason: '「我先不看」不该把里面跑着的东西停掉',
      );
      expect(
        c.read(layoutProvider).rightPanel,
        isNot(RightPanel.terminal),
        reason: '但那一列确实要让出来',
      );
    });

    test('关掉最后一个标签，整栏跟着让位', () {
      final c = _boot();
      addTearDown(c.dispose);
      final p = c.read(terminalPanelProvider.notifier);
      p.toggle();
      expect(c.read(layoutProvider).rightPanel, RightPanel.terminal);

      p.close(c.read(terminalPanelProvider).tabs.single.id);

      expect(c.read(terminalPanelProvider).tabs, isEmpty);
      expect(
        c.read(layoutProvider).rightPanel,
        isNot(RightPanel.terminal),
        reason: '留一块写着「一个终端都没有」的空面板占着三分之一屏，不如让位',
      );
      expect(
        c.read(terminalPanelProvider).expanded,
        isFalse,
        reason:
            '铺满状态要跟着清掉 —— 留着的话，下一次打开终端会突然盖住整个'
            '对话，而用户上一次点铺满是好几分钟前的事了',
      );
    });
  });

  group('顶栏那个按钮', () {
    test('第一次点：开一个标签并占住右侧那一列', () {
      final c = _boot();
      addTearDown(c.dispose);

      c.read(terminalPanelProvider.notifier).toggle();

      expect(c.read(terminalPanelProvider).tabs, hasLength(1));
      expect(c.read(layoutProvider).rightPanel, RightPanel.terminal);
    });

    test('再点一次是收起，且不动 shell', () {
      final c = _boot();
      addTearDown(c.dispose);
      final p = c.read(terminalPanelProvider.notifier);

      p.toggle();
      p.toggle();

      expect(c.read(layoutProvider).rightPanel, isNot(RightPanel.terminal));
      expect(
        c.read(terminalPanelProvider).tabs,
        hasLength(1),
        reason: '同一个按钮既是「给我看」也是「不看了」—— 但都不该是「都别跑了」',
      );
    });

    test('收起之后再点，回到原来那几个标签，不新开', () {
      final c = _boot();
      addTearDown(c.dispose);
      final p = c.read(terminalPanelProvider.notifier);
      p.toggle();
      p.addTab();
      p.toggle();

      p.toggle();

      expect(
        c.read(terminalPanelProvider).tabs,
        hasLength(2),
        reason: '每次打开都多一个标签的话，用完一天会攒出一排没人要的 shell',
      );
      expect(c.read(layoutProvider).rightPanel, RightPanel.terminal);
    });
  });

  test('换会话就清空 —— 那些 shell 站在错的目录里', () async {
    final c = _boot();
    addTearDown(c.dispose);
    // ⚠️ 先等开机那次会话列表落定。不等的话，mock 那几条会在中途到达
    // 并顺手选中第一条 —— 于是「换会话」在测试里发生了两次，而第二次
    // 不是这条用例干的
    c.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
    final deadline = DateTime.now().add(const Duration(seconds: 10));
    while (c.read(chatControllerProvider).sessionsLoading) {
      if (DateTime.now().isAfter(deadline)) fail('等开机那次会话列表超时');
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }

    // 先有一条会话，才谈得上「换」
    final chat = c.read(chatControllerProvider.notifier);
    final first = chat.createSession();
    // listen 是异步派发的，等它到
    await Future<void>.delayed(Duration.zero);

    final p = c.read(terminalPanelProvider.notifier);
    p.addTab();
    p.addTab();
    expect(c.read(terminalPanelProvider).sessionId, first);

    chat.createSession();
    await Future<void>.delayed(Duration.zero);

    expect(
      c.read(terminalPanelProvider).tabs,
      isEmpty,
      reason:
          '每个 shell 的 cwd 来自那条会话的工作区绑定。留着就是一排站在错'
          '目录里的 shell —— 而它们看起来完全正常，直到有人在里面写了个文件',
    );
    expect(
      c.read(terminalPanelProvider).nextNumber,
      1,
      reason: '新会话从「终端 1」重新数 —— 序号是这一条会话里的第几个',
    );
  });
}
