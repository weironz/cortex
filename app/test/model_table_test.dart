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
  group('模型列表', () {
    testWidgets('关掉的型号留在「未启用」组里，还带着判据', (tester) async {
      final s = ModelSource(
        id: 's',
        provider: 'openai',
        models: const ['a'],
        catalog: [
          _m('a', context: 1000000, input: 5000000, output: 30000000),
          _m('b', context: 400000, input: 750000, output: 4500000),
        ],
      );
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );

      expect(find.text('已启用（1）'), findsOneWidget);
      expect(
        find.text('未启用（1）'),
        findsOneWidget,
        reason:
            '这一组是服务端那个 `catalog` 字段解锁的。它要是空的，'
            '「关掉一个型号」就还是等于「删掉它」—— 想找回来得重新拉一次列表',
      );
      expect(
        find.textContaining(r'输入 $0.75/M · 输出 $4.50/M'),
        findsOneWidget,
        reason: '没开的那个也要带价格 —— 「该不该开它」的判据正是价格与上下文',
      );
      expect(find.text('400K'), findsOneWidget);
      expect(find.text('1M'), findsOneWidget);
    });

    testWidgets('价格与上下文查不到就留白，不画横杠', (tester) async {
      final s = ModelSource(
        id: 's',
        provider: 'custom',
        models: const ['x'],
        catalog: [_m('x')],
      );
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );

      expect(
        find.textContaining('—'),
        findsNothing,
        reason:
            '一个横杠在一列数字里读起来像「免费」或者「不支持」，'
            '而事实是「服务端目录里没有这个型号」。留白至少不说错话',
      );
      expect(find.textContaining('输入'), findsNothing);
    });

    testWidgets('只有一种模态时不画页签', (tester) async {
      final s = ModelSource(
        id: 's',
        provider: 'openai',
        models: const ['a'],
        catalog: [_m('a'), _m('b')],
      );
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );
      expect(
        find.textContaining('全部 ('),
        findsNothing,
        reason: '两个型号都是对话类，那一排页签点哪个结果都一样 —— 纯噪音',
      );
    });

    testWidgets('有图片模型时才出现「图片」页签，且按全集计数', (tester) async {
      final s = ModelSource(
        id: 's',
        provider: 'openai',
        models: const ['a'],
        catalog: [_m('a'), _m('img', image: true), _m('img2', unwired: true)],
      );
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );

      expect(find.text('全部 (3)'), findsOneWidget);
      expect(find.text('对话 (1)'), findsOneWidget);
      expect(
        find.text('图片 (2)'),
        findsOneWidget,
        reason:
            '`imageUnwired`（它会画，但我们没接这家）也算图片 —— '
            '归到「对话」里离事实更远，而 2026-08-20 用户正是因此来问'
            '「为什么这些模型不支持」',
      );
      // 视频 / 向量化 / ASR / TTS 四类我们**没有数据**。画四个恒为 0 的
      // 页签，是在说一个不成立的能力
      for (final absent in ['视频', '向量化', 'ASR', 'TTS']) {
        expect(
          find.textContaining(absent),
          findsNothing,
          reason: '$absent 我们判不出来，不画',
        );
      }
    });

    /// ⚠️ 老服务端（2026-08-21 之前）不下发 `catalog`。
    ///
    /// 直接画那个空列表的结果是「还没有型号，点获取模型列表」——
    /// 而用户上一分钟还在用这条来源聊天。一个新客户端连上老服务端就让人
    /// 以为配置丢了，是升级里最不该有的表现：他会去重填，
    /// 而重填修不好一个根本没坏的东西。
    testWidgets('老服务端没有 catalog 时，已配的型号照样画得出来', (tester) async {
      const s = ModelSource(
        id: 's',
        provider: 'alibaba',
        models: ['qwen-flash', 'qwen-plus'],
        // catalog 留空 = 老服务端的样子
      );
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );

      expect(find.text('已启用（2）'), findsOneWidget);
      expect(find.text('qwen-flash'), findsOneWidget);
      expect(
        find.textContaining('还没有型号'),
        findsNothing,
        reason: '它明明有两个型号在用 —— 说「还没有」会让人去重填一个没坏的配置',
      );
      // 兜底出来的条目只有 id，能力与价目一概不知道 —— 留白，不编
      expect(find.textContaining('输入'), findsNothing);
    });

    testWidgets('一个都没开时说清下一步，而不是只给一串灰的', (tester) async {
      final s = ModelSource(
        id: 's',
        provider: 'openai',
        catalog: [_m('a'), _m('b')],
      );
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );
      expect(find.text('已启用（0）'), findsOneWidget);
      expect(find.textContaining('在下面打开你要用的'), findsOneWidget);
    });

    /// ⚠️ 2026-08-21 在实物上撞到的一句假话。
    ///
    /// 判据一度是「过滤之后的已启用列表是不是空的」—— 于是搜一个不匹配
    /// 任何已启用型号的词，界面就写「一个都没开」，而它明明开着一个。
    /// 判据必须是**这条来源本身**有没有开着的（`models`），
    /// 与当前筛选无关。
    testWidgets('搜索把已启用的筛掉时，不能说「一个都没开」', (tester) async {
      final s = ModelSource(
        id: 's',
        provider: 'openai',
        models: const ['gpt-image-2'],
        catalog: [_m('gpt-image-2'), _m('gemini-3-pro')],
      );
      await _paint(
        tester,
        ModelTable(source: s, busy: false, onToggle: (_, _) {}, onFetch: () {}),
      );
      await tester.enterText(find.byType(TextField), 'gemini');
      await tester.pump();

      expect(find.text('已启用（0）'), findsOneWidget, reason: '当前筛选下确实是 0');
      expect(
        find.textContaining('一个都没开'),
        findsNothing,
        reason:
            '它开着 gpt-image-2，只是被筛掉了。说「一个都没开」会让人去'
            '重新打开一个本来就开着的型号',
      );
    });

    testWidgets('拨开关报的是那个型号的 id', (tester) async {
      final calls = <(String, bool)>[];
      final s = ModelSource(
        id: 's',
        provider: 'openai',
        models: const ['a'],
        catalog: [_m('a'), _m('b')],
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
      // 第二个开关 = 未启用组里的 b
      await tester.tap(find.byType(Switch).at(1));
      await tester.pump();
      expect(calls, [('b', true)]);
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
