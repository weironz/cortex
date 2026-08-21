/// 模型列表与服务商卡片墙 —— 2026-08-21 照 LobeHub 重做的那两块。
///
/// # 这里守的三条，每一条都对应一种「界面在撒谎」
///
/// 1. **不画我们答不出的东西。** 模态页签只出现在真的数得出来的那几档上；
///    价格与上下文查不到时留白，不画一个读起来像「免费」的横杠。
/// 2. **不画点不动的控件。** 还没配过 key 的供应商卡片上没有开关 ——
///    它启用不了，一个点了会弹窗的开关是让控件说了它做不到的事。
/// 3. **关掉的东西要还找得回来。** 「未启用」组必须真的画得出内容，
///    否则加 `catalog` 那个服务端字段就白做了。
library;

import 'package:cortex_app/features/settings/widgets/model_format.dart';
import 'package:cortex_app/features/settings/widgets/model_picker_drawer.dart';
import 'package:cortex_app/features/settings/widgets/model_table.dart';
import 'package:cortex_app/features/settings/widgets/provider_mark.dart';
import 'package:cortex_app/features/settings/widgets/provider_overview.dart';
import 'package:cortex_app/models/model_source.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

FetchedModel _m(
  String id, {
  int? context,
  int? input,
  int? output,
  bool? image,
  bool unwired = false,
  String display = '',
}) => FetchedModel(
  id: id,
  displayName: display,
  context: context,
  inputMicrosPerMtok: input,
  outputMicrosPerMtok: output,
  imageOutput: image,
  imageUnwired: unwired,
);

Future<void> _paint(WidgetTester tester, Widget child) async {
  tester.view.physicalSize = const Size(900, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: SingleChildScrollView(
          child: Padding(padding: const EdgeInsets.all(16), child: child),
        ),
      ),
    ),
  );
  await tester.pump();
}

void main() {
  group('模型列表（主列表只放已启用的）', () {
    testWidgets('只画已启用的，按系列分组', (tester) async {
      final s = ModelSource(
        id: 's',
        provider: 'openai',
        models: const ['gpt-4o', 'gpt-5', 'claude-3'],
        catalog: [
          _m('gpt-4o', context: 1000000, input: 5000000, output: 30000000),
          _m('gpt-5'),
          _m('claude-3'),
          // 这个**没加进来** —— 它只该出现在抽屉里
          _m('o1-preview'),
        ],
      );
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );

      expect(find.text('gpt（2）'), findsOneWidget);
      expect(find.text('claude（1）'), findsOneWidget);
      expect(
        find.text('o1-preview'),
        findsNothing,
        reason:
            '没加进来的不该出现在主列表里 —— 那正是这一版要解决的问题：'
            '281 个型号摊在这里，真正在用的那一两个会被埋掉',
      );
      expect(find.textContaining(r'输入 $5.00/M'), findsOneWidget);
      expect(find.text('1M'), findsOneWidget);
    });

    testWidgets('每行是「−」而不是开关', (tester) async {
      final calls = <(String, bool)>[];
      final s = ModelSource(
        id: 's',
        provider: 'openai',
        models: const ['gpt-4o'],
        catalog: [_m('gpt-4o')],
      );
      await _paint(
        tester,
        ModelTable(
          source: s,
          busy: false,
          onToggle: (id, on) => calls.add((id, on)),
          onFetch: () {},
        ),
      );

      expect(
        find.byType(Switch),
        findsNothing,
        reason: '这一列里每一个都是开着的 —— 一排恒为「开」的开关不传达任何信息，而它占的宽度不小',
      );
      await tester.tap(find.byIcon(Icons.remove_rounded));
      await tester.pump();
      expect(calls, [('gpt-4o', false)]);
    });

    testWidgets('拉过列表但一个都没加时，说清去哪儿加', (tester) async {
      final s = ModelSource(
        id: 's',
        provider: 'openai',
        catalog: [_m('a'), _m('b')],
      );
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );
      expect(find.textContaining('从右边挑几个加进来'), findsOneWidget);
    });

    testWidgets('还没拉过列表时，说的是另一句', (tester) async {
      const s = ModelSource(id: 's', provider: 'openai');
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );
      expect(
        find.textContaining('未必与你的账号一致'),
        findsOneWidget,
        reason:
            '「还没拉过」与「拉过但没加」是两件事，下一步也不一样 —— '
            '混成一句的话，前者会被指去一个空抽屉里挑',
      );
    });

    /// 老服务端（2026-08-21 之前）不下发 `catalog`，那时 `shownCatalog`
    /// 拿 `models` 兜底 —— 否则新客户端连上老服务端会让人以为配置丢了。
    testWidgets('老服务端没有 catalog 时，已配的型号照样画得出来', (tester) async {
      const s = ModelSource(
        id: 's',
        provider: 'alibaba',
        models: ['qwen-flash', 'qwen-plus'],
      );
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );
      expect(find.text('qwen（2）'), findsOneWidget);
      expect(find.text('qwen-flash'), findsOneWidget);
      expect(find.textContaining('还没有型号'), findsNothing);
    });
  });

  group('选型抽屉', () {
    ModelSource src() => ModelSource(
      id: 's',
      provider: 'openai',
      models: const ['gpt-4o'],
      catalog: [
        _m('gpt-4o', context: 1000000, input: 5000000, output: 30000000),
        _m('gpt-5'),
        _m('dall-e-3', image: true),
      ],
    );

    Future<void> open(
      WidgetTester tester,
      ModelSource s, {
      void Function(String, bool)? onToggle,
      void Function(List<String>)? onAddAll,
    }) => _paint(
      tester,
      SizedBox(
        height: 800,
        child: ModelPickerDrawer(
          title: 'OpenAI',
          provider: s.provider,
          catalog: s.shownCatalog,
          enabled: s.models,
          busy: false,
          onToggle: onToggle ?? (_, _) {},
          onAddAll: onAddAll ?? (_) {},
        ),
      ),
    );

    testWidgets('列全集，加过的显示「−」、没加的显示「+」', (tester) async {
      await open(tester, src());
      expect(find.text('gpt-4o'), findsOneWidget);
      expect(find.text('gpt-5'), findsOneWidget);
      expect(find.text('dall-e-3'), findsOneWidget);
      expect(
        find.byIcon(Icons.remove_rounded),
        findsOneWidget,
        reason: 'gpt-4o 已经加过了，它那一行该是「移除」',
      );
      expect(find.byIcon(Icons.add_rounded), findsNWidgets(2));
    });

    testWidgets('页签只画数得出来的那几档', (tester) async {
      await open(tester, src());
      expect(find.text('全部 3'), findsOneWidget);
      expect(find.text('对话 2'), findsOneWidget);
      expect(find.text('图片 1'), findsOneWidget);
      // Cherry 那排还有 嵌入/音频/视频/重排，**而且零也画**。
      // 我们四样数据都没有，画一个恒为 0 的页签是在说一个不成立的能力
      for (final absent in ['嵌入', '音频', '视频', '重排']) {
        expect(
          find.textContaining(absent),
          findsNothing,
          reason: '$absent 判不出来，不画',
        );
      }
    });

    testWidgets('「添加全部」只加当前筛选下看得见的', (tester) async {
      List<String>? got;
      await open(tester, src(), onAddAll: (ids) => got = ids);

      await tester.tap(find.text('图片 1'));
      await tester.pump();
      await tester.tap(find.textContaining('添加全部'));
      await tester.pump();

      expect(got, [
        'dall-e-3',
      ], reason: '筛了「图片」还把 gpt-5 一起加进去，就是拿一个用户没看过的清单替他做决定');
    });

    testWidgets('全加过之后「添加全部」按不动', (tester) async {
      const s = ModelSource(
        id: 's',
        provider: 'openai',
        models: ['a'],
        catalog: [FetchedModel(id: 'a')],
      );
      await open(tester, s);
      final btn = tester.widget<TextButton>(
        find.ancestor(
          of: find.textContaining('添加全部'),
          matching: find.byType(TextButton),
        ),
      );
      expect(btn.onPressed, isNull, reason: '一个点了没反应的按钮比没有这个按钮更让人困惑');
    });

    testWidgets('搜索按 id 匹配', (tester) async {
      await open(tester, src());
      await tester.enterText(find.byType(TextField), 'dall');
      await tester.pump();
      expect(find.text('dall-e-3'), findsOneWidget);
      expect(find.text('gpt-5'), findsNothing);
    });
  });

  group('服务商卡片墙', () {
    ModelSources data() => ModelSources(
      canAdd: true,
      sources: const [
        ModelSource(id: '1', provider: 'openai', label: '我的 OpenAI'),
        ModelSource(id: '2', provider: 'ollama', label: '本机', enabled: false),
      ],
      providers: const [
        ProviderChoice(id: 'openai', displayName: 'OpenAI'),
        ProviderChoice(id: 'ollama', displayName: 'Ollama'),
        ProviderChoice(
          id: 'deepseek',
          displayName: 'DeepSeek',
          description: '专注人工智能研究',
        ),
      ],
    );

    testWidgets('分两组，计数把「还没配过的供应商」也算进未启用', (tester) async {
      await _paint(
        tester,
        SizedBox(
          height: 700,
          child: ProviderOverview(
            data: data(),
            busy: false,
            query: '',
            onOpen: (_) {},
            onToggle: (_, _) {},
            onAdd: (_) {},
          ),
        ),
      );
      expect(find.text('已启用服务商'), findsOneWidget);
      expect(find.text('1'), findsOneWidget);
      // 关掉的 ollama + 还没配过的 deepseek
      expect(find.text('2'), findsOneWidget);
      expect(find.text('DeepSeek'), findsOneWidget);
    });

    testWidgets('还没配过的那张卡上**没有**开关', (tester) async {
      await _paint(
        tester,
        SizedBox(
          height: 700,
          child: ProviderOverview(
            data: data(),
            busy: false,
            query: '',
            onOpen: (_) {},
            onToggle: (_, _) {},
            onAdd: (_) {},
          ),
        ),
      );
      final card = find.byKey(const ValueKey('card:new:deepseek'));
      expect(card, findsOneWidget);
      expect(
        find.descendant(of: card, matching: find.byType(Switch)),
        findsNothing,
        reason:
            '没有 key 的供应商是启用不了的。画一个点了会弹「添加」对话框的'
            '开关，是让控件说了一件它做不到的事',
      );
      // 有来源撑着的那两张则必须有
      for (final id in ['1', '2']) {
        expect(
          find.descendant(
            of: find.byKey(ValueKey('card:$id')),
            matching: find.byType(Switch),
          ),
          findsOneWidget,
        );
      }
    });

    testWidgets('搜索同时过滤这面墙 —— 否则读起来像搜索没生效', (tester) async {
      await _paint(
        tester,
        SizedBox(
          height: 700,
          child: ProviderOverview(
            data: data(),
            busy: false,
            query: 'deep',
            onOpen: (_) {},
            onToggle: (_, _) {},
            onAdd: (_) {},
          ),
        ),
      );
      expect(find.text('DeepSeek'), findsOneWidget);
      expect(find.text('我的 OpenAI'), findsNothing);
    });
  });

  group('品牌标', () {
    testWidgets('没有资源时退回首字母，而不是一块空白', (tester) async {
      await _paint(
        tester,
        const ProviderMark(
          provider: 'nope-no-such-brand',
          displayName: '自定义',
          size: 24,
        ),
      );
      // 资源加载是异步的，errorBuilder 要下一帧才接管
      await tester.pump();
      expect(
        find.text('自'),
        findsOneWidget,
        reason:
            '「自定义」那家本来就没有品牌，长尾供应商也没有 —— '
            '兜底不是临时方案，是这条路的另一半',
      );
    });
  });

  group('格式化', () {
    test('上下文查不到回空串，不回横杠', () {
      expect(formatContextTokens(null), '');
      expect(formatContextTokens(0), '');
      expect(formatContextTokens(400000), '400K');
      expect(formatContextTokens(1000000), '1M');
      expect(formatContextTokens(1500000), '1.5M');
    });

    test('价格半行不如不画', () {
      expect(
        formatPricePair(_m('x', input: 5000000)),
        '',
        reason: '只有输入价没有输出价时说半句，比不说更容易被读成「输出免费」',
      );
      expect(
        formatPricePair(_m('x', input: 5000000, output: 30000000)),
        r'输入 $5.00/M · 输出 $30.00/M',
      );
    });

    test('模态判据把「会画但我们没接」也算图片', () {
      expect(ModelKind.of(_m('a')), ModelKind.chat);
      expect(ModelKind.of(_m('a', image: true)), ModelKind.image);
      expect(ModelKind.of(_m('a', unwired: true)), ModelKind.image);
    });
  });
}
