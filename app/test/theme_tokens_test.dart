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

      test('外壳保持低色度的冷中性灰，接近白色', () {
        // 允许极轻的冷灰蓝，但不允许某个表面单独偏成高色度背景。
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
            lessThanOrEqualTo(8),
            reason: '$label 色度过高，会让文字和表面显得杂乱',
          );
          expect(
            color.b,
            greaterThanOrEqualTo(color.r),
            reason: '$label 偏暖，不符合白色与冷中性的外观方向',
          );
        }
      });

      test('侧栏的选中态是中性的', () {
        expect(
          _chroma(tokens.sidebarAccent),
          lessThanOrEqualTo(12),
          reason:
              '「当前在看哪个会话」是**位置**，不是动作。此前这里是 '
              'primary.withValues(alpha: .12)，于是整条侧栏常年挂着一块紫，'
              '把真正需要注意的东西一起稀释掉',
        );
        expect(
          (_luma(tokens.sidebarAccent) - _luma(tokens.sidebar)).abs(),
          greaterThan(4),
          reason: '与侧栏底色差得太小，选中态就看不出来',
        );
      });

      /// ⚠️ **选中态必须与悬停态拉开，否则它就是「一闪而过」。**
      ///
      /// 2026-10-23 用户实报：「切换会话时，定位会话的灰色背景只会一闪而过，
      /// 并不会停留在光标所在会话上」。
      ///
      /// 状态层是好的（`activeSessionId` 一直是对的，见
      /// `session_selection_test.dart`）—— 坏的是**对比度**：上一版浅色
      /// `sidebarAccent = #EBEBED` 与 sidebar 只差 6，而悬停是它 55% 的
      /// 叠色（差 3）。于是点下去那一刻屏幕上的变化只有 3 个亮度单位，
      /// 鼠标一移开又回到 6 —— 人眼读到的就是「亮了一下，没了」。
      ///
      /// 这条钉的是**两个差值同时成立**：选中 vs 侧栏要够，选中 vs 悬停
      /// 也要够。只钉前者的话，悬停调到与选中一样深照样能过，而那时
      /// 鼠标扫过每一行都像被选中了。
      test('选中态与悬停态拉得开（否则就是「一闪而过」）', () {
        final base = _luma(tokens.sidebar);
        final selected = _luma(tokens.sidebarAccent);
        // 悬停是 sidebarAccent 在 45% 处叠在 sidebar 上 —— 与
        // `_SessionTile` 里那个 `withValues(alpha: 0.45)` 是同一个数。
        // 两处不一致的话这条测试量的不是屏幕上那个颜色
        final hover = base + (selected - base) * 0.45;

        expect(
          (selected - base).abs(),
          greaterThan(10),
          reason:
              '选中与侧栏只差 ${(selected - base).abs().toStringAsFixed(1)} 个亮度单位。'
              '上一版是 6，用户看到的就是「灰色背景一闪而过」',
        );
        expect(
          (selected - hover).abs(),
          greaterThan(4),
          reason:
              '选中与悬停只差 ${(selected - hover).abs().toStringAsFixed(1)}。'
              '差得太小时，鼠标移开那一刻看起来像「选中态消失了」',
        );
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

      test('边框是实色发丝线，不用透明度', () {
        // 2026-10-23 重设计把这条**翻转**：透明度造出来的线在不同表面上
        // 深浅不一（白底上一条线、fill 上另一条线）；实色线在每个表面上
        // 都是同一条线 —— 这正是「发丝线」语言的成立前提
        for (final (label, c) in [
          ('outline', scheme.outline),
          ('outlineVariant', scheme.outlineVariant),
        ]) {
          expect(
            c.a,
            1.0,
            reason:
                '$label 是半透明的。半透明的线在白底、fill、侧栏上深浅不一，'
                '「每个表面上都是同一条线」就不成立了',
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

  /// 裸主题下取 `cortex` **不许炸**。
  ///
  /// 写成 `extension<CortexTokens>()!` 的那一版，任何一个把部件挂在默认
  /// `MaterialApp` 下的 widget 测试都会当场
  /// 「Null check operator used on a null value」—— 而报错指向那个部件，
  /// 跟主题看不出关系。2026-08-21 真红过一次（offline_visibility_test）。
  testWidgets('没挂扩展的主题下也取得到，不炸', (tester) async {
    late CortexTokens got;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) {
            got = Theme.of(context).cortex;
            return const SizedBox.shrink();
          },
        ),
      ),
    );
    expect(got.sidebar, isNotNull);
    expect(
      got.foregroundTertiary,
      isNotNull,
      reason: '推出来的那份不追求好看，只保证在任何主题下都说得通',
    );
  });

  test('圆角是一条非递减的阶梯', () {
    // 2026-10-23 重设计：卡片与对话框同档（12/12，Linear 一路）——
    // 「浮起的窗口」与「贴地的卡片」共用一档是刻意的，收紧的圆角体系
    // 里多一档反而没有消费面。所以阶梯从「严格递增」放宽为
    // 「非递减」，且相邻档差不许倒挂
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
        greaterThanOrEqualTo(steps[i - 1]),
        reason: '阶梯倒挂了，「紧凑控件用小的、容器用大的」这条规则没法执行',
      );
    }
  });
}
