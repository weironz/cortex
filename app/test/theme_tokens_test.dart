/// 主题里那几条**跨页面成立**的规矩。
///
/// # 为什么这些值得测
///
/// 它们全是「改坏了不报错，只是看起来有点不对」的那一类：某个表面被顺手
/// 调亮一点、某个第三级前景改成了半透明、深色下弹层比卡片更暗。每一条
/// 单独看都无伤大雅，合起来就是设计规范逐渐失效 —— 而没有任何测试会红。
///
/// 规范本身在 [docs/design.md](../../docs/design.md)。
library;

import 'package:cortex_app/core/theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 感知亮度，用来比较两个中性色谁更亮。
double _luma(Color c) =>
    0.2126 * (c.r * 255) + 0.7152 * (c.g * 255) + 0.0722 * (c.b * 255);

/// 这个颜色离「无彩」有多远。0 = 纯灰。
int _chroma(Color c) {
  final r = (c.r * 255).round();
  final g = (c.g * 255).round();
  final b = (c.b * 255).round();
  return [r, g, b].reduce((a, x) => a > x ? a : x) -
      [r, g, b].reduce((a, x) => a < x ? a : x);
}

void main() {
  for (final (name, theme) in [
    ('浅色', CortexTheme.light()),
    ('深色', CortexTheme.dark()),
  ]) {
    group(name, () {
      final scheme = theme.colorScheme;
      final tokens = theme.cortex;

      test('扩展挂上去了', () {
        expect(
          theme.extension<CortexTokens>(),
          isNotNull,
          reason:
              '忘了挂扩展的表现不是编译失败，而是每一处 `Theme.of(context).cortex` '
              '在运行时炸掉 —— 而那要打开对应界面才看得见',
        );
      });

      test('外壳是无彩的', () {
        // 正文与背景允许极小的偏差（排版惯例），但不该是一个"有颜色的灰"
        for (final (label, color) in [
          ('surface', scheme.surface),
          ('surfaceContainer', scheme.surfaceContainer),
          ('onSurface', scheme.onSurface),
          ('onSurfaceVariant', scheme.onSurfaceVariant),
          ('sidebar', tokens.sidebar),
          ('foregroundTertiary', tokens.foregroundTertiary),
        ]) {
          expect(
            _chroma(color),
            lessThanOrEqualTo(2),
            reason:
                '$label 是有彩的。偏冷/偏暖的灰与真正的彩色并排时会被读成'
                '「一个没调准的颜色」，而不是「无色」—— 这正是这一版要去掉的东西',
          );
        }
      });

      test('侧栏与内容区分得开', () {
        expect(
          (_luma(tokens.sidebar) - _luma(scheme.surface)).abs(),
          greaterThan(6),
          reason:
              '侧栏是**独立表面**，靠底色与内容区分开。差得太小就等于没分，'
              '那时又要回去加分隔线和阴影',
        );
      });

      test('前景三级是拉开的，且都不透明', () {
        final l1 = _luma(scheme.onSurface);
        final l2 = _luma(scheme.onSurfaceVariant);
        final l3 = _luma(tokens.foregroundTertiary);
        // 浅色下越次要越浅，深色下越次要越暗 —— 方向相反，比的是「离正文更远」
        expect((l2 - l1).abs(), greaterThan(20), reason: '第二级与正文分不开，等于只有一级');
        expect(
          (l3 - l1).abs(),
          greaterThan((l2 - l1).abs()),
          reason: '第三级必须比第二级离正文更远，否则这一级是白设的',
        );
        for (final (label, c) in [
          ('onSurfaceVariant', scheme.onSurfaceVariant),
          ('foregroundTertiary', tokens.foregroundTertiary),
        ]) {
          expect(
            c.a,
            1.0,
            reason:
                '$label 是半透明的。半透明造出来的层级在不同表面上深浅不一，'
                '而且会与「这个控件被禁用了」的半透明撞在一起',
          );
        }
      });

      test('边框用透明度，不用实色', () {
        for (final (label, c) in [
          ('outline', scheme.outline),
          ('outlineVariant', scheme.outlineVariant),
        ]) {
          expect(
            c.a,
            lessThan(1.0),
            reason:
                '$label 是实色。实色边框只在它被调出来的那一个表面上好看，'
                '换到卡片、弹层、侧栏上就会偏亮或偏暗',
          );
        }
      });
    });
  }

  test('深色下层越高越亮', () {
    final s = CortexTheme.dark().colorScheme;
    expect(
      _luma(s.surfaceContainerHighest),
      greaterThan(_luma(s.surfaceContainer)),
      reason:
          '弹层坐在卡片**之上**，就该更亮。反过来做会让浮起来的东西看着陷进去 —— '
          '那是把浅色模式的直觉原样搬过来的结果',
    );
    expect(
      _luma(s.surfaceContainer),
      greaterThan(_luma(s.surface)),
      reason: '卡片在底板之上',
    );
  });

  test('圆角是一条递增的阶梯', () {
    const steps = [
      CortexTokens.radiusSm,
      CortexTokens.radiusMd,
      CortexTokens.radiusLg,
      CortexTokens.radiusXl,
      CortexTokens.radius2xl,
    ];
    for (var i = 1; i < steps.length; i++) {
      expect(
        steps[i],
        greaterThan(steps[i - 1]),
        reason: '阶梯要单调递增，否则「紧凑控件用小的、容器用大的」这条规则没法执行',
      );
    }
  });
}
