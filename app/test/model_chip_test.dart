/// 撰写框那个模型 chip，和供应商下拉。
///
/// 盯住的四件事：
///
/// 1. **随手关掉面板会不会静默改掉选择** —— 「跟随部署」这一项要 pop 一个
///    null，而点面板外关掉给到的也是 null。撞车的表现是「本来选着 Flash，
///    误触一下就退回默认了」，而用户不知道自己改过什么
/// 2. **chip 与设置页是不是同一份状态** —— 两份的表现是「chip 上显示 A，
///    设置里显示 B，发出去的是其中一个」
/// 3. **供应商是不是下拉** —— 手打就是在猜我们内部的 id
/// 4. **免 key 的那家还逼不逼人填 key**
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/model_chip.dart';
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

/// `instant: true` —— 去掉 mock 的假延迟，否则树销毁时会被判成
/// 「A Timer is still pending」，而报错里一个字都没提是哪个请求。
ProviderContainer _boot() => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
    // **必须替掉**：不替的话这些测试会去读开发机上真实的 settings.json，
    // 成败取决于跑它的那台机器上选过什么模型
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
  ],
);

/// 点面板里的一项。
///
/// **要先 `ensureVisible`。** 底部面板在测试里可以排到视口之外
/// （实测 y=1903 而视口高 1600），那时 `tap` 的坐标不落在任何 widget 上，
/// 它只打印一行 Warning 就过去了 —— 测试于是「点了但什么都没发生」，
/// 而失败信息里一个字都没提是没点到。
Future<void> _tapInSheet(WidgetTester tester, Finder f) async {
  await tester.ensureVisible(f);
  await tester.pump(const Duration(milliseconds: 200));
  await tester.tap(f);
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

Future<void> _pump(
  WidgetTester tester,
  ProviderContainer c,
  Widget child,
) async {
  tester.view.physicalSize = const Size(1000, 1600);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: MaterialApp(home: Scaffold(body: child)),
    ),
  );
  // FutureProvider 要好几轮微任务才落地；一次 pump 不够，而
  // pumpAndSettle 在 mock 后端还有在飞定时器时不收敛
  for (var i = 0; i < 8; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

void main() {
  group('撰写框上的模型 chip', () {
    testWidgets('显示的是当前选择，点开能换', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c, const ModelChip());

      // 没选过 → 显示部署默认那个的显示名
      expect(find.text('DeepSeek V4 Pro'), findsOneWidget);

      await tester.tap(find.byType(ModelChip));
      await tester.pump(const Duration(milliseconds: 400));
      await _tapInSheet(tester, find.text('DeepSeek V4 Flash').last);

      expect(
        c.read(selectedModelProvider).model,
        'deepseek-v4-flash',
        reason:
            'chip 换了模型却没写进 selectedModelProvider 的话，'
            '显示是新的、发出去的是旧的',
      );
    });

    testWidgets('随手关掉面板不改动已有的选择', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      c
          .read(selectedModelProvider.notifier)
          .select(
            const ModelPick(source: 'deployment', model: 'deepseek-v4-flash'),
          );
      await _pump(tester, c, const ModelChip());

      await tester.tap(find.byType(ModelChip));
      await tester.pump(const Duration(milliseconds: 400));
      // 点面板外面 = 取消
      await tester.tapAt(const Offset(20, 20));
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }

      expect(
        c.read(selectedModelProvider).model,
        'deepseek-v4-flash',
        reason:
            '取消与「选了跟随部署」都回 null 的话，误触一下就静默退回默认了，'
            '而用户完全不知道自己改过什么',
      );
    });

    testWidgets('选择器按来源分组，一个型号名离开来源没有意义', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c, const ModelChip());

      await tester.tap(find.byType(ModelChip));
      await tester.pump(const Duration(milliseconds: 400));

      // mock 里两条启用的来源：部署提供（占配额）与 alibaba（你的 key）
      expect(find.text('部署提供'), findsOneWidget);
      expect(
        find.text('占配额'),
        findsOneWidget,
        reason: '「用这个会不会花我的额度」要在选之前就看得见',
      );
      expect(find.text('你的 key'), findsWidgets);
      expect(
        find.text('Qwen Flash'),
        findsOneWidget,
        reason:
            '别的来源的型号也要能挑到 —— 从前这份列表只有部署那家的，'
            '于是配了 alibaba key 的人选什么都被服务端 400 拒',
      );
    });

    testWidgets('选中带来源的型号之后，两段都记下来', (tester) async {
      final c = _boot();
      addTearDown(c.dispose);
      await _pump(tester, c, const ModelChip());

      await tester.tap(find.byType(ModelChip));
      await tester.pump(const Duration(milliseconds: 400));
      await _tapInSheet(tester, find.text('Qwen Flash'));

      final got = c.read(selectedModelProvider);
      expect(got.model, 'qwen-flash');
      expect(
        got.source,
        '01M0MOCKSOURCEAAAAAAAAAAAA',
        reason: '只记型号不记来源的话，同一个名字在两条来源上都有时就分不清了',
      );
    });
  });
}
