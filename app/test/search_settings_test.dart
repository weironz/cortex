/// 设置 → 联网检索。
///
/// # 这一组盯着三件**不会报错地错**的事
///
/// 1. **「能不能搜」的判据有两半。** 选了一家但没填 key 时，服务端
///    `SearchPrefs::resolve` **不会**回落到部署那把 —— 所以「选了一家」
///    并不等于「能用」。判据少一半的话，界面会说「正在用博查」，
///    而模型手上根本没有这个工具。
/// 2. **key 那个框留空 = 不动，不是清掉。** 界面从来拿不到明文，
///    于是「只改一下结果个数」如果把 key 一起发成空串，用户的 key 就没了，
///    而他没有明文可以补回来。
/// 3. **抓正文不是每家都行。** 博查只做搜索，选它之后 `web_fetch` 用不了，
///    而这件事必须在设置页上说清 —— 否则用户会以为那个工具坏了。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/settings/pages/search_page.dart';
import 'package:cortex_app/models/search_prefs.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

const _tavily = SearchProviderInfo(
  id: 'tavily',
  name: 'Tavily',
  defaultBase: 'https://api.tavily.com',
  canFetch: true,
);
const _bocha = SearchProviderInfo(
  id: 'bocha',
  name: '博查',
  defaultBase: 'https://api.bochaai.com',
  canFetch: false,
);
const _exa = SearchProviderInfo(
  id: 'exa',
  name: 'Exa',
  defaultBase: 'https://api.exa.ai',
  canFetch: true,
);

Future<void> _pump(WidgetTester tester, SearchPrefs prefs) async {
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        searchPrefsProvider.overrideWith((ref) async => prefs),
      ],
      child: const MaterialApp(
        home: Scaffold(
          body: SizedBox(width: 900, height: 900, child: SearchPage()),
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 100));
}

/// 滚到底。
///
/// ⚠️ 这一页是个 `ListView`，而它**懒构建** —— 视口外的那几节根本不在
/// 树上，于是 `find` 找不到它们，测试会红在「文案不见了」上，而实际只是
/// 没滚到。光把窗口造得很高也不够（ListView 仍按视口构建）。
Future<void> _toBottom(WidgetTester tester) async {
  await tester.drag(find.byType(ListView), const Offset(0, -2000));
  await tester.pump();
}

void main() {
  group('「能不能搜」这一行', () {
    test('判据的两半都要看', () {
      // 没配过、部署也没 key
      expect(
        const SearchPrefs(providers: [_tavily]).usable,
        isFalse,
        reason: '两边都没有 key 时模型手上没有这个工具，界面不能说「在用」',
      );
      // 没配过，但部署有
      expect(
        const SearchPrefs(deploymentKey: true, providers: [_tavily]).usable,
        isTrue,
      );
      // ⚠️ 选了一家却没填 key —— 服务端**不会**回落到部署那把
      expect(
        const SearchPrefs(
          provider: 'bocha',
          deploymentKey: true,
          providers: [_tavily, _bocha],
        ).usable,
        isFalse,
        reason:
            '这一条是这组里最要紧的：少了它，界面会说「正在用博查」，'
            '而服务端那侧解不出配置，模型仍然搜不了',
      );
      // 选了一家并填了
      expect(
        const SearchPrefs(
          provider: 'bocha',
          keyTail: 'abcd',
          providers: [_tavily, _bocha],
        ).usable,
        isTrue,
      );
    });

    testWidgets('还没接上时说清「模型此刻没有这个工具」', (tester) async {
      await _pump(tester, const SearchPrefs(providers: [_tavily]));
      expect(
        find.textContaining('模型此刻没有联网检索这个工具'),
        findsOneWidget,
        reason: '一句含糊的「未配置」不会让人知道要做什么',
      );
    });

    testWidgets('选了一家却没填 key 时，说清不会回落', (tester) async {
      await _pump(
        tester,
        const SearchPrefs(
          provider: 'bocha',
          deploymentKey: true,
          providers: [_tavily, _bocha],
        ),
      );
      expect(
        find.textContaining('不会回落到部署那把'),
        findsOneWidget,
        reason: '不说的话，用户以为「反正还有部署那把兜着」，而实际上搜不了',
      );
    });
  });

  group('抓正文那一节', () {
    testWidgets('选了只做搜索的那家，要说清 web_fetch 用不了', (tester) async {
      await _pump(
        tester,
        const SearchPrefs(
          provider: 'bocha',
          keyTail: 'abcd',
          providers: [_tavily, _bocha],
        ),
      );
      await _toBottom(tester);
      expect(
        find.textContaining('只做搜索，不抓正文'),
        findsOneWidget,
        reason: '不说的话用户会以为 web_fetch 坏了',
      );
      expect(
        find.textContaining('Tavily'),
        findsWidgets,
        reason: '要说清换成哪家才抓得了 —— 只说「不行」等于把问题丢回去',
      );
    });

    /// ⚠️ **换了下拉框但还没点保存时，说的必须是新选的那家。**
    ///
    /// 2026-08-27 在桌面端实地撞到：这一节按**草稿** id 判断「选了没有」，
    /// 却按**已保存**的 id 去查那一家的能力。两者不一致时查不到，于是回落成
    /// 「原始 id + 抓不了」——屏幕上是
    /// 「⚠️ exa 只做搜索，不抓正文……要抓正文的话，换成：Tavily、Exa」，
    /// **同一句话既说它不行又叫你换成它**。
    testWidgets('刚换下拉框还没保存时，说的是新选的那家', (tester) async {
      await _pump(
        tester,
        const SearchPrefs(
          deploymentKey: true,
          providers: [_tavily, _bocha, _exa],
        ),
      );

      // 从「部署提供」切到 Exa，**不点保存**
      await tester.tap(find.text('部署提供').last);
      await tester.pumpAndSettle();
      await tester.tap(find.text('Exa').last);
      await tester.pumpAndSettle();

      await _toBottom(tester);
      expect(
        find.textContaining('只做搜索，不抓正文'),
        findsNothing,
        reason: 'Exa 有 /contents，抓得了 —— 这句话是照着「已保存」那个 id 查空之后的回落',
      );
      expect(
        find.textContaining('exa 只做搜索'),
        findsNothing,
        reason: '小写的原始 id 本身就是「查空了」的痕迹：认出来的话该显示「Exa」',
      );
      expect(
        find.textContaining('Exa 抓得了网页正文'),
        findsOneWidget,
        reason: '要说清换过去之后 web_fetch 还能用',
      );
    });

    /// 换到只做搜索的那家时，同样要按**新选的**那家说 —— 方向相反的一条，
    /// 防的是「干脆一律说抓得了」这种把上面那条骗过去的改法。
    testWidgets('刚换到只做搜索的那家，警告立刻出现', (tester) async {
      await _pump(
        tester,
        const SearchPrefs(
          provider: 'tavily',
          keyTail: 'abcd',
          providers: [_tavily, _bocha, _exa],
        ),
      );

      await tester.tap(find.text('Tavily').last);
      await tester.pumpAndSettle();
      await tester.tap(find.text('博查').last);
      await tester.pumpAndSettle();

      await _toBottom(tester);
      expect(
        find.textContaining('博查 只做搜索，不抓正文'),
        findsOneWidget,
        reason: '还没保存也要按新选的那家说，否则用户是照着旧状态做决定',
      );
    });

    testWidgets('选了抓得了的那家，不摆警告', (tester) async {
      await _pump(
        tester,
        const SearchPrefs(
          provider: 'tavily',
          keyTail: 'abcd',
          providers: [_tavily, _bocha],
        ),
      );
      await _toBottom(tester);
      expect(find.textContaining('只做搜索，不抓正文'), findsNothing);
    });
  });

  group('key 那个框', () {
    testWidgets('已经填过时，占位符显示后四位而不是明文', (tester) async {
      await _pump(
        tester,
        const SearchPrefs(
          provider: 'tavily',
          keyTail: 'wxyz',
          providers: [_tavily],
        ),
      );
      expect(
        find.textContaining('wxyz'),
        findsWidgets,
        reason: '用户要能认出「填的是哪一把」，而我们永远拿不到明文',
      );
      expect(
        find.textContaining('留空 = 不动它'),
        findsOneWidget,
        reason: '不说的话，用户会以为不填就会被清掉，于是每次改别的设置都重贴一遍 key',
      );
    });

    testWidgets('选「部署提供」时不摆 key 与地址那两个框', (tester) async {
      await _pump(
        tester,
        const SearchPrefs(deploymentKey: true, providers: [_tavily]),
      );
      expect(
        find.textContaining('API 密钥'),
        findsNothing,
        reason: '那一档下这两个框点了没用 —— 摆一个点了没用的框比不摆更糟',
      );
      expect(find.textContaining('API 地址'), findsNothing);
    });
  });
}
