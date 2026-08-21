/// 动效三档。
///
/// # 这三档到底在防什么
///
/// 系统那一位（`MediaQuery.disableAnimations`）是**一个二元表态**，而它
/// 表达不了两种真实需求：「系统开着减少动效，但我在这个应用里想要」，
/// 以及「系统没开，但我不想要」。三档的价值全在**两个方向都能覆盖**——
/// 只做「关闭」那一档会漏掉前一半。
///
/// 所以这里的断言里有一半是关于 [MotionPref.full] 的：那一档看起来像
/// 「默认」，实际是唯一能推翻系统的那个方向。
library;

import 'package:cortex_app/core/motion.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('三档怎么落成一位', () {
    test('跟随系统 —— 两个方向都跟', () {
      expect(
        resolveDisableAnimations(MotionPref.system, system: true),
        isTrue,
        reason: '系统说减少动效时必须减少。这一条红了，就是我们在无视一个无障碍设置',
      );
      expect(
        resolveDisableAnimations(MotionPref.system, system: false),
        isFalse,
        reason: '系统没开时不该自作主张关掉动效',
      );
    });

    test('标准 —— 即使系统说减少，也要动效', () {
      expect(
        resolveDisableAnimations(MotionPref.full, system: true),
        isFalse,
        reason:
            '**这一条是三档存在的一半理由。**系统开着「减少动效」但用户在这个应用里'
            '明确选了「标准」，就该有动效 —— 红了的话这一档等于不存在，'
            '而它与「跟随系统」在系统没开时行为相同，界面上完全看不出来',
      );
      expect(resolveDisableAnimations(MotionPref.full, system: false), isFalse);
    });

    test('关闭 —— 即使系统没开，也不要动效', () {
      expect(
        resolveDisableAnimations(MotionPref.reduced, system: false),
        isTrue,
        reason: '这是三档的另一半理由：系统里没表达过，但在这个应用里想关掉',
      );
      expect(
        resolveDisableAnimations(MotionPref.reduced, system: true),
        isTrue,
      );
    });
  });

  group('存下来的字符串', () {
    test('每一档都存得下、读得回', () {
      for (final p in MotionPref.values) {
        expect(
          MotionPref.fromWire(p.wire),
          p,
          reason: '${p.name} 存进去读出来变了 —— 用户下次启动会发现自己的选择被改了',
        );
      }
    });

    test('认不出来的一律回「跟随系统」', () {
      for (final raw in [null, '', 'agile', 'AGILE', '3', 'system ']) {
        expect(
          MotionPref.fromWire(raw),
          MotionPref.system,
          reason:
              '「$raw」应该退回跟随系统。**不能退回 full** —— '
              '那等于拿一个读坏的配置文件去推翻用户在系统里表达过的无障碍偏好',
        );
      }
    });
  });

  group('落盘', () {
    test('选过的档位存得进去', () async {
      final written = <String, String>{};
      final c = ProviderContainer(
        overrides: [
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((v) async {
            written.addAll(v);
          }),
        ],
      );
      addTearDown(c.dispose);

      c.read(motionPrefProvider.notifier).set(MotionPref.reduced);
      // patcher 是读—改—写，排在一条 future 链上
      await Future<void>.delayed(Duration.zero);

      expect(
        written['motion'],
        'reduced',
        reason: '不落盘的话，一个关掉动效的人每次启动都要再关一遍 —— 而这正是无障碍设置最不该有的负担',
      );
    });

    test('启动时读得回来', () async {
      final c = ProviderContainer(
        overrides: [
          settingsReaderProvider.overrideWithValue(
            () async => const {'motion': 'reduced'},
          ),
        ],
      );
      addTearDown(c.dispose);

      expect(
        c.read(motionPrefProvider),
        MotionPref.system,
        reason: '读盘是异步的，同步的 build 只能先给默认值',
      );
      await Future<void>.delayed(Duration.zero);
      expect(
        c.read(motionPrefProvider),
        MotionPref.reduced,
        reason: '读不回来的话「存下来」就没有意义 —— 症状与压根没存一模一样',
      );
    });

    test('主题也存得进去 —— 它此前完全活在内存里', () async {
      final written = <String, String>{};
      final c = ProviderContainer(
        overrides: [
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((v) async {
            written.addAll(v);
          }),
        ],
      );
      addTearDown(c.dispose);

      c.read(themeModeProvider.notifier).set(ThemeMode.dark);
      await Future<void>.delayed(Duration.zero);

      expect(
        written['theme_mode'],
        'dark',
        reason: '不存的话，调成深色的人每次启动都会被白底闪一下，然后自己点回去',
      );
    });
  });

  group('三档真的传到了 widget 树里', () {
    /// 这一组测的是**接线**，不是 `resolveDisableAnimations` 的逻辑：
    /// 那个纯函数上面已经测过了，而它算对了却没接上去的情形
    /// （改写在错误的层、被 `MaterialApp` 自己的 MediaQuery 盖掉）
    /// 是这个功能唯一会静默失效的方式。
    testWidgets('选「关闭」时，后代读到的 reducedMotion 为真', (tester) async {
      late bool seen;
      await tester.pumpWidget(
        ProviderScope(
          overrides: [
            settingsReaderProvider.overrideWithValue(
              () async => const {'motion': 'reduced'},
            ),
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
                home: Builder(
                  builder: (context) {
                    seen = reducedMotion(context);
                    return const SizedBox.shrink();
                  },
                ),
              );
            },
          ),
        ),
      );
      // 读盘的那个微任务落定之后重建
      await tester.pumpAndSettle();

      expect(
        seen,
        isTrue,
        reason:
            '后代读不到的话，这个开关只是在 provider 里改了个值 —— '
            '三个点照转、侧栏照渐变，而设置页上写着「关闭」',
      );
    });

    testWidgets('转场时长在关闭时归零，而不只是变短', (tester) async {
      late Duration d;
      await tester.pumpWidget(
        MediaQuery(
          data: const MediaQueryData(disableAnimations: true),
          child: Builder(
            builder: (context) {
              d = motionDuration(context, const Duration(milliseconds: 160));
              return const SizedBox.shrink();
            },
          ),
        ),
      );
      expect(d, Duration.zero, reason: '「短一点」仍然是一次会动的位移 —— 而位移正是前庭敏感最难受的一类');
    });
  });
}
