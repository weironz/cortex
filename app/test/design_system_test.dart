/// 设计系统那一层自己的规矩。
///
/// # 为什么这些值得测
///
/// 它们全是「改坏了不报错，只是看起来有点不对」的那一类：某个间距多写
/// 2px、某个动效时长绕过了「减少动效」开关、悬停态提亮得看不见。
/// 每一条单独看都无伤大雅，合起来就是设计规范逐渐失效 —— 而没有任何
/// 测试会红。
///
/// 颜色与圆角的规矩在 `theme_tokens_test.dart`，这里管的是**非颜色**那一半。
library;

import 'package:cortex_app/design_system/design_system.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('间距阶', () {
    test('除 xxs / sm 外全是 4 的倍数', () {
      // 两档写明的例外：xxs = 2（胶囊内上下那一点点呼吸）、
      // sm = 6（同一个东西内部的两截）。除此之外任何不是 4 的倍数的值，
      // 都是「觉得 12 有点小、16 有点大」的手感产物 —— 而手感一旦被允许，
      // 清点时就分不出哪一处是例外
      const steps = {
        'xs': CortexSpace.xs,
        'md': CortexSpace.md,
        'lg': CortexSpace.lg,
        'xl': CortexSpace.xl,
        'xxl': CortexSpace.xxl,
        'xxxl': CortexSpace.xxxl,
      };
      for (final MapEntry(key: name, value: v) in steps.entries) {
        expect(
          v % 4,
          0,
          reason:
              '$name = $v 不是 4 的倍数。间距阶是 4 的倍数，'
              '只有 xxs(2) 与 sm(6) 是写明的例外',
        );
      }
      expect(
        CortexSpace.sm % 4,
        isNot(0),
        reason:
            'sm 若变成了 4 的倍数，它就不再是「内部两截」那一档，'
            '而那一档是它存在的全部理由',
      );
    });

    test('阶是严格递增的', () {
      const steps = [
        CortexSpace.xxs,
        CortexSpace.xs,
        CortexSpace.sm,
        CortexSpace.md,
        CortexSpace.lg,
        CortexSpace.xl,
        CortexSpace.xxl,
        CortexSpace.xxxl,
      ];
      for (var i = 1; i < steps.length; i++) {
        expect(
          steps[i],
          greaterThan(steps[i - 1]),
          reason: '阶梯倒挂了，「就近取阶」这条规则没法执行',
        );
      }
    });

    test('行内边距左边比右边宽', () {
      // 行尾通常挂着一个 26 宽的图标按钮，视觉重心要往左找回来一点
      expect(
        CortexSpace.row.left,
        greaterThan(CortexSpace.row.right),
        reason: '左右相等时，标题看起来贴着左边、按钮却离右边很远',
      );
    });
  });

  group('动效', () {
    testWidgets('「减少动效」开着时全部解析成 0', (tester) async {
      late Duration fast;
      late Duration slow;
      await tester.pumpWidget(
        MediaQuery(
          data: const MediaQueryData(disableAnimations: true),
          child: Builder(
            builder: (context) {
              fast = CortexMotion.resolve(context, CortexMotion.fast);
              slow = CortexMotion.resolve(context, CortexMotion.slow);
              return const SizedBox.shrink();
            },
          ),
        ),
      );
      expect(fast, Duration.zero, reason: '「短一点」仍然是一次会动的位移，而位移正是前庭敏感最难受的一类');
      expect(slow, Duration.zero);
    });

    testWidgets('没开时原样返回', (tester) async {
      late Duration fast;
      await tester.pumpWidget(
        MediaQuery(
          data: const MediaQueryData(disableAnimations: false),
          child: Builder(
            builder: (context) {
              fast = CortexMotion.resolve(context, CortexMotion.fast);
              return const SizedBox.shrink();
            },
          ),
        ),
      );
      expect(fast, CortexMotion.fast);
    });

    test('时长是递增的几档', () {
      const steps = [
        CortexMotion.instant,
        CortexMotion.fast,
        CortexMotion.base,
        CortexMotion.slow,
      ];
      for (var i = 1; i < steps.length; i++) {
        expect(
          steps[i],
          greaterThan(steps[i - 1]),
          reason: '时长档倒挂，「越大的事动得越久」这条规则没法执行',
        );
      }
    });
  });

  group('原子件', () {
    Widget wrap(Widget child, {ThemeData? theme, bool reduce = false}) =>
        MaterialApp(
          theme: theme ?? CortexTheme.dark(),
          home: MediaQuery(
            data: MediaQueryData(disableAnimations: reduce),
            child: Scaffold(body: Center(child: child)),
          ),
        );

    testWidgets('面板常态是发丝线，悬停后边框提亮', (tester) async {
      await tester.pumpWidget(
        wrap(
          CortexSurface(
            onTap: () {},
            padding: const EdgeInsets.all(CortexSpace.lg),
            child: const Text('面板'),
          ),
        ),
      );

      BoxDecoration deco() {
        final box = tester.widget<AnimatedContainer>(
          find.byType(AnimatedContainer).first,
        );
        return box.decoration! as BoxDecoration;
      }

      final rest = deco();
      expect(
        rest.border,
        isNotNull,
        reason: '面板必须有边框 —— 这一套设计的层级靠色差 + 1px 线，不靠阴影',
      );
      expect(
        (rest.border! as Border).top.width,
        CortexSpace.hair,
        reason: '常态线宽必须是发丝线（1px）',
      );

      final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await mouse.addPointer(location: Offset.zero);
      addTearDown(mouse.removePointer);
      await mouse.moveTo(tester.getCenter(find.text('面板')));
      await tester.pumpAndSettle();

      final hot = deco();
      expect(
        (hot.border! as Border).top.color,
        isNot((rest.border! as Border).top.color),
        reason: '悬停时边框没有提亮 —— 那这一行「能点」这件事就只有指针形状在说',
      );
    });

    testWidgets('面板常态没有投影，浮起才有', (tester) async {
      await tester.pumpWidget(wrap(const CortexSurface(child: Text('常态'))));
      var deco =
          tester
                  .widget<AnimatedContainer>(
                    find.byType(AnimatedContainer).first,
                  )
                  .decoration!
              as BoxDecoration;
      expect(
        deco.boxShadow,
        isNull,
        reason:
            '静态层级靠表面色阶，不用阴影。阴影一多整屏会发灰，'
            '「浮起来」这个信号也随之失效',
      );

      await tester.pumpWidget(
        wrap(const CortexSurface(floating: true, child: Text('浮起'))),
      );
      deco =
          tester
                  .widget<AnimatedContainer>(
                    find.byType(AnimatedContainer).first,
                  )
                  .decoration!
              as BoxDecoration;
      expect(deco.boxShadow, isNotNull, reason: '浮层该有投影');
    });

    testWidgets('图标按钮停用时仍然占位', (tester) async {
      await tester.pumpWidget(
        wrap(
          const CortexIconButton(
            icon: Icons.copy_rounded,
            tooltip: '复制',
            onPressed: null,
          ),
        ),
      );
      final box = tester.getSize(find.byType(AnimatedContainer).first);
      expect(box.width, 26, reason: '停用而不是消失：按钮忽隐忽现会让这一行的宽度跳一下');
      expect(find.byIcon(Icons.copy_rounded), findsOneWidget);
    });

    testWidgets('胶囊的高度由内容撑出来，不写死', (tester) async {
      await tester.pumpWidget(wrap(const CortexPill(label: '已完成 3 次')));
      final h = tester.getSize(find.byType(Container).last).height;
      expect(h, greaterThan(16), reason: '写死高度之后，换字体或换语言就会截字');
    });
  });
}
