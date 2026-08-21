/// 图片页 —— 画一张、翻画廊、点开那五个动作。
///
/// # 这一组在盯的三件事
///
/// 1. **规格面板只说当下真成立的话。** 尺寸在自定义端点上只是提示词里的
///    一句话，数量在非 DashScope 的来源上是服务端连发凑的 —— 两句提示
///    漏掉任何一句，界面就在承诺一件它做不到的事（CLAUDE.md 第 2 条）。
/// 2. **渲染冒烟。** 「外观」那一页曾经一片空白，起因是渲染期异常不变红。
///    这里每个形态都 `takeException()` 一次。
/// 3. **「以此为提示词重画」真的把那句话填回去了。** 它是查看器里唯一一个
///    有后续动作的按钮，断掉的表现是点了没反应。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/images/image_page.dart';
import 'package:cortex_app/features/images/widgets/image_spec.dart';
import 'package:cortex_app/models/model_option.dart';
import 'package:cortex_app/models/model_source.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/model_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

ProviderContainer _boot() => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
  ],
);

Future<void> _pump(WidgetTester tester, ProviderContainer c) async {
  tester.view.physicalSize = const Size(1100, 1400);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: const MaterialApp(home: Scaffold(body: ImagePage())),
    ),
  );
  // FutureProvider 要好几轮微任务才落地；pumpAndSettle 在 mock 后端
  // 还有在飞定时器时不收敛
  for (var i = 0; i < 10; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

Future<void> _settle(WidgetTester tester, [int rounds = 12]) async {
  for (var i = 0; i < rounds; i++) {
    await tester.pump(const Duration(milliseconds: 120));
  }
}

void main() {
  group('图片页', () {
    testWidgets('空画廊说清楚要做什么，而不是给一片空白', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c);

      expect(
        tester.takeException(),
        isNull,
        reason:
            '渲染期异常不会让测试自己变红 —— 画面上只是一片空白，'
            '「外观」那一页就是这么漏过去的',
      );
      expect(find.text('还没有画过图'), findsOneWidget);
      expect(find.text('描述新图片…'), findsOneWidget);
      // 型号与规格两个 chip 都在
      expect(find.byKey(const ValueKey('chip:model')), findsOneWidget);
      expect(find.byKey(const ValueKey('chip:spec')), findsOneWidget);
    });

    testWidgets('画一张之后它出现在墙上', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.enterText(find.byType(TextField).first, '一只戴眼镜的柴犬');
      await tester.tap(find.byTooltip('生成'));
      await _settle(tester, 20);

      expect(tester.takeException(), isNull);
      expect(
        find.byType(Image),
        findsWidgets,
        reason:
            '画完要重取画廊并画出来 —— 只回一句「成功」而墙上什么都没多，'
            '用户没法确认它真画了',
      );
      expect(find.text('还没有画过图'), findsNothing, reason: '墙上有东西了，空态必须让位');
    });

    testWidgets('画成了才清空输入框', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.enterText(find.byType(TextField).first, '一只柴犬');
      await tester.tap(find.byTooltip('生成'));
      await _settle(tester, 20);

      final field = tester.widget<TextField>(find.byType(TextField).first);
      expect(
        field.controller?.text,
        isEmpty,
        reason: '画成了就清空；失败时留着，那时用户最不该被迫重打一遍',
      );
    });

    testWidgets('点开大图，「以此为提示词重画」把那句话填回输入框', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c);

      await tester.enterText(find.byType(TextField).first, '水彩风格的柴犬');
      await tester.tap(find.byTooltip('生成'));
      await _settle(tester, 20);

      await tester.tap(find.byType(InkWell).last);
      await _settle(tester);
      expect(tester.takeException(), isNull);
      expect(
        find.byKey(const ValueKey('viewer:reprompt')),
        findsOneWidget,
        reason:
            '这个按钮不叫「编辑」—— 我们没有 img2img，'
            '叫编辑用户会以为是在这张图上改',
      );

      await tester.tap(find.byKey(const ValueKey('viewer:reprompt')));
      await _settle(tester);

      final field = tester.widget<TextField>(find.byType(TextField).first);
      expect(
        field.controller?.text,
        '水彩风格的柴犬',
        reason:
            '这个动作的全部意义就是把提示词拿回来改一改 —— '
            '填不回去就是点了没反应',
      );
    });
  });

  group('「自动」档也要说实话', () {
    /// 一条中转站来源 + 它上面一个能生图的型号。
    ///
    /// 这就是 2026-08-22 那台机器的真实配置 —— 用户唯一能画图的来源，
    /// 而它恰恰是尺寸最不作数的那一种。
    ModelSources gatewayOnly() => const ModelSources(
      sources: [
        ModelSource(
          id: 'src-gw',
          provider: 'openai',
          baseUrl: 'https://gw.example.com/v1',
          models: ['gpt-image-2'],
        ),
      ],
    );

    ModelSources dashscopeOnly() => const ModelSources(
      sources: [
        ModelSource(
          id: 'src-ali',
          provider: 'alibaba',
          models: ['qwen-image-3.0'],
        ),
      ],
    );

    ModelCatalog catalogFor(String sourceId, String modelId) => ModelCatalog(
      models: [
        ModelOption(
          id: modelId,
          displayName: modelId,
          source: sourceId,
          imageOutput: true,
        ),
      ],
    );

    Future<void> pumpWith(
      WidgetTester tester,
      ModelSources sources,
      ModelCatalog catalog,
    ) async {
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((_) async {}),
          modelSourcesProvider.overrideWith((ref) async => sources),
          modelCatalogProvider.overrideWith((ref) async => catalog),
        ],
      );
      addTearDown(c.dispose);
      await _pump(tester, c);
      // 打开规格面板
      await tester.tap(find.byKey(const ValueKey('chip:spec')));
      await _settle(tester);
    }

    testWidgets('只有中转站能画时，「自动」也提醒尺寸只是尽力而为', (tester) async {
      await pumpWith(
        tester,
        gatewayOnly(),
        catalogFor('src-gw', 'gpt-image-2'),
      );
      expect(
        find.byKey(const ValueKey('note:size-best-effort')),
        findsOneWidget,
        reason:
            '「自动」是默认档，而这台机器上它必定落到那条中转站上 —— '
            '「没指名」不等于「没有事实可说」，那一句必须出',
      );
    });

    testWidgets('只有 DashScope 能画时，「自动」不说那句多余的话', (tester) async {
      await pumpWith(
        tester,
        dashscopeOnly(),
        catalogFor('src-ali', 'qwen-image-3.0'),
      );
      expect(
        find.byKey(const ValueKey('note:size-best-effort')),
        findsNothing,
        reason:
            '候选只有一条走官方接口的，尺寸是真参数 —— '
            '再提醒一次是在给一个没有的问题预警',
      );
    });
  });

  group('规格面板只说真成立的话', () {
    Future<void> pumpSheet(
      WidgetTester tester, {
      required bool nativeMultiImage,
      required bool customEndpoint,
      int count = 1,
    }) async {
      tester.view.physicalSize = const Size(900, 1400);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.reset);
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: ImageSpecSheet(
              size: null,
              count: count,
              nativeMultiImage: nativeMultiImage,
              customEndpoint: customEndpoint,
            ),
          ),
        ),
      );
      await tester.pump();
    }

    testWidgets('自定义端点上标明尺寸只是尽力而为', (tester) async {
      await pumpSheet(tester, nativeMultiImage: false, customEndpoint: true);
      expect(
        find.byKey(const ValueKey('note:size-best-effort')),
        findsOneWidget,
        reason:
            '中转站那条只能把尺寸拼进提示词，对方认不认不由我们说 —— '
            '不说的话，「我设了 1024 却不是」看起来就是个 bug',
      );
    });

    testWidgets('走官方接口的来源上不说那句话', (tester) async {
      await pumpSheet(tester, nativeMultiImage: true, customEndpoint: false);
      expect(
        find.byKey(const ValueKey('note:size-best-effort')),
        findsNothing,
        reason:
            'DashScope 那条尺寸是真参数，多说一句「未必是这个尺寸」'
            '是在给一个没有的问题预警',
      );
    });

    testWidgets('数量 > 1 且这条来源一次只出一张时，说清是连发凑的', (tester) async {
      await pumpSheet(
        tester,
        nativeMultiImage: false,
        customEndpoint: true,
        count: 3,
      );
      expect(
        find.byKey(const ValueKey('note:fanout')),
        findsOneWidget,
        reason:
            '连发 3 次就是 3 次调用、3 份钱、3 倍的等待 —— '
            '这三件事都要说在按下去之前',
      );
    });

    testWidgets('一次能出多张的来源上不说连发', (tester) async {
      await pumpSheet(
        tester,
        nativeMultiImage: true,
        customEndpoint: false,
        count: 3,
      );
      expect(find.byKey(const ValueKey('note:fanout')), findsNothing);
    });

    testWidgets('只画一张时不说连发，但钱那句一直在', (tester) async {
      await pumpSheet(tester, nativeMultiImage: false, customEndpoint: true);
      expect(find.byKey(const ValueKey('note:fanout')), findsNothing);
      expect(
        find.byKey(const ValueKey('note:cost')),
        findsOneWidget,
        reason: '生图按张计费这件事，一张的时候也该知道',
      );
    });

    testWidgets('质量与背景两个控件不画', (tester) async {
      await pumpSheet(tester, nativeMultiImage: true, customEndpoint: false);
      // ChatGPT 那个面板上有这两项，而我们三条协议都没有对应参数 ——
      // 画出来就是界面替模型答应了一件它做不到的事
      expect(find.text('质量'), findsNothing);
      expect(find.text('背景'), findsNothing);
    });
  });
}
