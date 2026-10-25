/// 界面缩放与聊天文字大小。
///
/// # 这组测试在防什么
///
/// 两个旋钮都有「值算对了却没生效」的静默失效方式：改写层级放错
/// （`MediaQuery` 被 `MaterialApp` 自己的盖掉）、聊天档乘错了基准
/// （把进到这一层之前的系数顶掉，平台偏好丢失）、落盘解析不吸附
/// （档位之外的值让选择卡片没有一个亮着）。纯函数、落盘、接线
/// 三段各管一种。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/ui_scale.dart';
import 'package:cortex_app/features/settings/pages/appearance_page.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import 'settings_pages_test.dart' show paintWith;

void main() {
  group('档位解析', () {
    test('没存过 / 坏了 → null（调用方落回 1.0）', () {
      for (final raw in [null, '', 'abc', '0', '-1.5', 'NaN', '1e400']) {
        expect(
          parseScale(raw),
          isNull,
          reason: '「$raw」不该被当成一个档位 —— 那会让应用以未知的大小启动',
        );
      }
    });

    test('档位之内的原样回来', () {
      for (final p in kScalePresets) {
        expect(parseScale(p), p);
        expect(parseScale('$p'), p, reason: '落盘存的是字符串');
      }
    });

    test('档位之外的吸附到最近的一档', () {
      // 设置页只提供档位；存一个档位之外的值（手改过 settings.json、
      // 或者旧版本存过别的档）会让选择卡片没有一个亮着 —— 而那六张卡
      // 是这一页唯一的可见状态
      expect(parseScale(1.2), 1.25, reason: '1.2 离 1.25 比离 1.1 近');
      expect(parseScale(1.44), 1.5);
      expect(parseScale(2.5), 1.75, reason: '越上界吸附到最大档，而不是原样放行');
      expect(parseScale(0.1), 0.9, reason: '越下界吸附到最小档');
    });
  });

  group('落盘', () {
    test('两个旋钮各存各的键', () async {
      // **按次收集**，不是累积成一个 map：patcher 是「读—改—写」，一次
      // patch 只带**被改的那一个键**。累积成 map 的话，「没改过的键不该
      // 被写」这条断言永远测不出来 —— 上一次的写入还躺在里面
      final writes = <Map<String, String>>[];
      final c = ProviderContainer(
        overrides: [
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((v) async {
            writes.add(Map.of(v));
          }),
        ],
      );
      addTearDown(c.dispose);

      c.read(uiScalePrefsProvider.notifier).setUi(1.25);
      await Future<void>.delayed(Duration.zero);
      c.read(uiScalePrefsProvider.notifier).setChat(1.5);
      await Future<void>.delayed(Duration.zero);

      expect(writes[0]['ui_scale'], '1.25');
      expect(writes[0]['chat_text_scale'], isNull);
      expect(writes[1]['chat_text_scale'], '1.5');
      expect(
        writes[1]['ui_scale'],
        isNull,
        reason: '改聊天档不许把界面档一起写 —— 两个旋钮各自管各自的事',
      );
    });

    test('启动时两个都读得回来', () async {
      final c = ProviderContainer(
        overrides: [
          settingsReaderProvider.overrideWithValue(
            () async => const {'ui_scale': '1.25', 'chat_text_scale': '1.5'},
          ),
        ],
      );
      addTearDown(c.dispose);

      expect(c.read(uiScalePrefsProvider), const UiScale());
      await Future<void>.delayed(Duration.zero);
      expect(
        c.read(uiScalePrefsProvider),
        const UiScale(ui: 1.25, chat: 1.5),
        reason: '读不回来的话「存下来」就没有意义',
      );
    });
  });

  group('接线', () {
    /// 复刻 `app.dart` 的接线（界面档在 builder、聊天档在子树），
    /// 然后量后代真实生效的字号。
    ///
    /// 这组测的是**接线**：纯函数错了上面会红，但「算对了却改写错了层」
    /// 是这个功能唯一会静默失效的方式 —— 与动效三档那组同一条纪律。
    Future<double> pumpAndMeasure(
      WidgetTester tester, {
      required double ui,
      required double chat,
    }) async {
      double? seen;
      Widget measure() => Builder(
        builder: (context) {
          seen =
              Theme.of(context).textTheme.bodyMedium!.fontSize! *
              MediaQuery.textScalerOf(context).scale(14) /
              14;
          return const SizedBox.shrink();
        },
      );
      await tester.pumpWidget(
        ProviderScope(
          overrides: [
            uiScalePrefsProvider.overrideWith(() => _Fixed(ui, chat)),
          ],
          child: MaterialApp(
            builder: (context, child) {
              final media = MediaQuery.of(context);
              return MediaQuery(
                data: media.copyWith(
                  textScaler: composeTextScaler(
                    scaleFactorOf(media.textScaler),
                    ui,
                  ),
                ),
                child: child ?? const SizedBox.shrink(),
              );
            },
            // 聊天档的子树改写与 chat_pane.dart 同构：**乘在进到这一层
            // 之前的系数上**。chat > 1 才包这一层，顺带把「没有聊天档时
            // 子树 override 不存在」的分支也覆盖到
            home: chat <= 1.0
                ? measure()
                : Builder(
                    builder: (context) => MediaQuery(
                      data: MediaQuery.of(context).copyWith(
                        textScaler: composeTextScaler(
                          scaleFactorOf(MediaQuery.textScalerOf(context)),
                          chat,
                        ),
                      ),
                      child: measure(),
                    ),
                  ),
          ),
        ),
      );
      return seen!;
    }

    testWidgets('界面档 125% 时，全局文字 1.25 倍', (tester) async {
      final size = await pumpAndMeasure(tester, ui: 1.25, chat: 1.0);
      expect(
        size,
        closeTo(14 * 1.25, 0.01),
        reason: '界面档必须真的到达后代 —— 改写层级放错的话这里恒为 14',
      );
    });

    testWidgets('聊天档乘在界面档之上', (tester) async {
      final size = await pumpAndMeasure(tester, ui: 1.25, chat: 1.5);
      expect(
        size,
        closeTo(14 * 1.25 * 1.5, 0.01),
        reason: '聊天区的最终缩放 = 界面 × 聊天 —— 少乘一边这里立刻红',
      );
    });

    testWidgets('两档都在默认时，字号一个不动', (tester) async {
      final size = await pumpAndMeasure(tester, ui: 1.0, chat: 1.0);
      expect(size, 14.0, reason: '加了设置之后默认观感必须与从前一致');
    });
  });

  group('设置页', () {
    testWidgets('外观 —— 两节缩放选择卡都画得出来', (tester) async {
      final err = await paintWith(tester, const AppearancePage());
      expect(err, isNull, reason: '渲染期约束错误不冒红，必须主动取一次');
      expect(find.text('界面缩放'), findsOneWidget);
      expect(find.text('聊天文字大小'), findsOneWidget);
      // 六个档位 × 两行都在
      expect(find.text('175%'), findsNWidgets(2));
      expect(find.text('90%'), findsNWidgets(2));
    });

    testWidgets('外观 —— 点 125% 会写盘并亮起那张卡', (tester) async {
      final writes = <Map<String, String>>[];
      final container = ProviderContainer(
        overrides: [
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((v) async {
            writes.add(Map.of(v));
          }),
          cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
        ],
      );
      addTearDown(container.dispose);

      tester.view.physicalSize = const Size(900, 1400);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.reset);

      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: container,
          child: const MaterialApp(home: Scaffold(body: AppearancePage())),
        ),
      );
      await tester.pump();
      await tester.pump();

      // 这一节排在主题与密度之后，初始视口里未必可见 —— 不滚的话
      // tap 会打在空白处，然后「点了没反应」
      await tester.ensureVisible(find.text('125%').first);
      await tester.pump();
      await tester.tap(find.text('125%').first);
      // ⚠️ **这里不能 `await Future.delayed(zero)`。** testWidgets 的假时钟
      // 里，零时长 timer 要到下一次 pump 才走 —— 直接 await 它等于把这个
      // 测试挂到天荒地老（实测：10 分钟超时，中间一行输出都没有）。
      // patcher 的写是一条微任务链，pump 两次足够它落定
      await tester.pump();
      await tester.pump();

      expect(writes.last['ui_scale'], '1.25');
      expect(
        container.read(uiScalePrefsProvider).ui,
        1.25,
        reason: '点了卡片状态没变的话，用户看到的「当前」与实际生效的永远差一拍',
      );
    });
  });
}

/// 档位在 notifier 里被钉死的替身 —— 接线测试不想依赖读盘的异步时序。
class _Fixed extends UiScaleNotifier {
  _Fixed(this.ui, this.chat);
  final double ui;
  final double chat;

  @override
  UiScale build() => UiScale(ui: ui, chat: chat);
}
