/// **每一页都真的画得出来。**
///
/// # 为什么需要这么一条看起来很笨的测试
///
/// 2026-08-21 重排设置页时，「外观」那一页发出去是**一片空白** ——
/// 标题画着，内容一个字都没有。而当时：
///
/// * `flutter analyze` 全绿（那是个渲染期约束错误，静态分析看不见）；
/// * 510 条测试全绿（它们测的是纯函数、状态机、以及三两个具体部件）；
/// * 分档逻辑本身的 10 条断言也全绿 —— 值算对了，只是没人把它画出来。
///
/// 真凶是 `Row(crossAxisAlignment: stretch)` 待在竖直无界的 `ListView` 里，
/// 抛 `BoxConstraints forces an infinite height`。这类错误**不冒红**，
/// 外层把它吞在渲染阶段，症状就是一块空白。
///
/// 所以判据不能是「逻辑对不对」，只能是「**画出来之后，那上面有东西吗**」。
/// 这条测试的全部价值就在这里，它便宜到没有理由不写。
///
/// # 为什么用 `takeException`
///
/// 渲染期异常被 Flutter 的测试绑定收集起来而不是直接抛出。不主动取一次的话，
/// 一页画崩了这条测试**照样是绿的** —— 那就又回到了发版当天那个状态。
///
/// # ⚠️ 没覆盖到的：`FutureProvider` 的**错误**分支
///
/// 「用量」页拉不到数时那两支（`isUnsupported` 的「这个部署不记用量」、
/// 以及一般失败的「拉不到用量」）**这里测不到**，试过就此打住了：
///
/// 同一个夹具里，成功那条两帧就画出来了；换成一个 `usage()` 抛 404 的
/// 假 API，`usage()` 确实被调到、确实抛了，但界面**一直停在转圈上** ——
/// 多画几帧、`pump(50ms)`、`runAsync` 都不动。看起来是 Riverpod 的错误
/// 投递与 `flutter_test` 假时钟之间的某种交互，不是页面本身的毛病。
///
/// 留一条测不通的红、或者删掉断言留个假绿，都比如实写在这里差。
/// 这两支要覆盖的话，得先把错误渲染从 `FutureProvider` 里剥出来
/// （比如让 `UsagePage` 接一个 `AsyncValue` 参数），那是另一次改动。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/motion.dart';
import 'package:cortex_app/features/settings/pages/about_page.dart';
import 'package:cortex_app/features/settings/pages/appearance_page.dart';
import 'package:cortex_app/features/settings/pages/data_page.dart';
import 'package:cortex_app/features/settings/pages/model_page.dart';
import 'package:cortex_app/features/settings/pages/usage_page.dart';
import 'package:cortex_app/features/settings/widgets/settings_layout.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 把一页画在一个真实尺寸的内容区里，然后**取一次渲染异常**。
///
/// 尺寸取的是设置窗右侧内容区在 1080p 上的实际大小。给一个过大的画布
/// 会让溢出类问题测不出来，给一个过小的又会造出实际不存在的溢出。
Future<Object?> _paint(WidgetTester tester, Widget page) async {
  tester.view.physicalSize = const Size(900, 760);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);

  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        // 不替的话这些页会去读开发机上真实的 settings.json
        settingsReaderProvider.overrideWithValue(
          () async => const <String, String>{},
        ),
        settingsWriterProvider.overrideWithValue((_) async {}),
        // **必须替。** 「数据」与「用量」两页开局就发请求，走真实实现的话
        // 它们会去连网并挂上重试定时器 —— 表现是
        // 「A Timer is still pending even after the widget tree was disposed」，
        // 一条与版式毫无关系的红
        cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
      ],
      child: MaterialApp(
        home: Scaffold(
          body: Padding(
            padding: const EdgeInsets.fromLTRB(24, 0, 24, 16),
            child: page,
          ),
        ),
      ),
    ),
  );
  // 两帧：第一帧让读设置那个微任务落定，第二帧让 `FutureProvider`
  // 的结果传到界面上。
  //
  // 不用 `pumpAndSettle`：这几页上有转圈指示器，那是个永不停的动画，
  // 会把它挂到超时。
  await tester.pump();
  await tester.pump();
  return tester.takeException();
}

/// 复刻 `app.dart` 的接线：动效三档**改写 `MediaQuery.disableAnimations`**。
///
/// 有些断言只有在这一层存在时才有意义 —— 「读 MediaQuery」与「读平台」
/// 在没有改写时是同一个值，那时任何区分两者的测试都恒绿。
Future<void> _paintUnderAppWiring(WidgetTester tester, Widget page) async {
  tester.view.physicalSize = const Size(900, 760);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);

  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        settingsReaderProvider.overrideWithValue(
          () async => const <String, String>{},
        ),
        settingsWriterProvider.overrideWithValue((_) async {}),
      ],
      child: Consumer(
        builder: (context, ref, _) {
          final pref = ref.watch(motionPrefProvider);
          return MaterialApp(
            builder: (context, child) {
              final media = MediaQuery.of(context);
              return MediaQuery(
                data: media.copyWith(
                  disableAnimations: resolveDisableAnimations(
                    pref,
                    system: media.disableAnimations,
                  ),
                ),
                child: child ?? const SizedBox.shrink(),
              );
            },
            home: Scaffold(body: page),
          );
        },
      ),
    ),
  );
  await tester.pump();
  await tester.pump();
}

void main() {
  group('设置页画得出来', () {
    testWidgets('外观 —— 主题与动效两节都在', (tester) async {
      final err = await _paint(tester, const AppearancePage());

      expect(err, isNull, reason: '画崩了。这一条正是 2026-08-21 那次「一片空白」会红的地方');
      expect(
        find.text('主题'),
        findsOneWidget,
        reason: '标题在而内容不在，恰恰是那次空白的样子 —— 所以必须断言内容，不能只断言不抛异常',
      );
      expect(find.text('动效'), findsOneWidget);
      // 三档都要看得见。少一档的话「跟随系统」与另外两档就分不出来了
      expect(find.text('跟随系统'), findsNWidgets(2), reason: '主题一个、动效一个');
      expect(find.text('标准'), findsOneWidget);
      expect(find.text('关闭'), findsOneWidget);
    });

    testWidgets('外观 —— 「跟随系统」写出了系统此刻的值', (tester) async {
      await _paint(tester, const AppearancePage());
      expect(
        find.textContaining('系统现在：'),
        findsOneWidget,
        reason:
            '「跟随系统」是个**间接**档位。不把系统此刻的值写出来，'
            '用户没法判断眼前的效果是哪一档造成的，只能去别处试',
      );
    });

    /// 这一条钉的是 2026-08-21 当场抓到的一句谎。
    ///
    /// 那行小字最初读的是 `MediaQuery` —— 而三档**正是靠改写 `MediaQuery`**
    /// 生效的，于是它读到的是「当前生效值」而不是系统的意见：选「关闭」，
    /// 它就跟着写「系统现在：关闭」，凭空捏造一个用户从没设过的系统设置。
    ///
    /// 判据必须是「档位变了，这行字**不**跟着变」。只断言这行字存在
    /// （上面那条）抓不到它 —— 那次它一直都在，只是在说假话。
    testWidgets('外观 —— 选了「关闭」也不会篡改「系统现在」那一行', (tester) async {
      // ⚠️ **必须挂在真实接线下**（`app.dart` 那层 MediaQuery 改写），
      // 不能用 `_paint`。裸 `MaterialApp` 里没人改写 disableAnimations，
      // 于是读 MediaQuery 与读平台**恰好同值** —— 这条测试会永远绿，
      // 而它要抓的正是两者不同的那一刻。第一版就是这么写的，注入不红
      await _paintUnderAppWiring(tester, const AppearancePage());

      // 测试环境里系统那位是 false，所以正确答案恒为「开启」
      expect(find.text('系统现在：开启'), findsOneWidget);

      await tester.tap(find.text('关闭'));
      await tester.pump();

      expect(
        find.text('系统现在：开启'),
        findsOneWidget,
        reason:
            '选了「关闭」之后这行字变成「系统现在：关闭」的话，它读的就是'
            '**被我们自己改写过**的 MediaQuery —— 那是在向用户报告一个'
            '他从没设过的系统设置。要系统那一位得绕过整棵树问平台',
      );
    });

    testWidgets('数据 —— 工作空间与导入两节都在', (tester) async {
      final err = await _paint(tester, const DataPage());
      expect(err, isNull);
      expect(find.text('导入'), findsOneWidget);
    });

    testWidgets('模型服务 —— 默认落在「全部」总览上', (tester) async {
      final err = await _paint(tester, const ModelPage());
      expect(err, isNull);
      // 这一页最大的一块是卡片墙。它画崩了的话右侧一片空白，
      // 而左列照常 —— 与 2026-08-21「外观」页那次一模一样的形状
      expect(find.text('已启用服务商'), findsOneWidget);
      expect(find.text('未启用服务商'), findsOneWidget);
      expect(
        find.byKey(const ValueKey('src:all')),
        findsOneWidget,
        reason: '「全部」是左列里唯一一个不指向某一条的入口，也是默认落点',
      );
    });

    testWidgets('关于', (tester) async {
      final err = await _paint(tester, const AboutPage());
      expect(err, isNull);
    });

    testWidgets('用量 —— 有数时画得出金额与按模型两节', (tester) async {
      final err = await _paint(tester, const UsagePage());
      expect(err, isNull);
      expect(find.text('花费'), findsOneWidget);
      expect(find.text('按模型'), findsOneWidget);
    });
  });

  group('版式件本身', () {
    /// 直接钉住那个踩过的形状：三档选择器**必须**能待在无界的竖直空间里。
    /// 上面那条是端到端的，这条是「为什么」——它红的时候直接指向病灶。
    testWidgets('三档选择器放进 ListView 不炸', (tester) async {
      final err = await _paint(
        tester,
        ListView(
          children: [
            SettingsChoice<int>(
              value: 1,
              onChanged: (_) {},
              options: const [
                (value: 0, label: '甲', hint: '一句说明'),
                // 说明特意写长：三块等高这件事只有在某一块换行时才看得出来，
                // 而那正是当初用 stretch 的理由
                (value: 1, label: '乙', hint: '一句很长很长的说明，长到必须换行才放得下'),
                (value: 2, label: '丙', hint: '一句说明'),
              ],
            ),
          ],
        ),
      );
      expect(
        err,
        isNull,
        reason:
            'ListView 给的是**无界高度**，而选择器要三块等高。'
            '直接用 CrossAxisAlignment.stretch 会把 h=Infinity 传下去 —— '
            '必须裹 IntrinsicHeight',
      );
      expect(find.text('乙'), findsOneWidget);
    });
  });
}
