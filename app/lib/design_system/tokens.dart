/// 设计系统的**非颜色**那一半：间距、字号角色、动效、发丝线、投影。
///
/// # 为什么单独一层，而不是继续往 `core/theme.dart` 里塞
///
/// `theme.dart` 回答的是「**这个角色该是什么颜色/字号**」—— 它必须能被
/// `ThemeData` 承载，所以只能装 `ColorScheme` 与 `TextTheme` 装得下的东西。
/// 而这一层回答的是「**两个东西之间隔多少、动多久、线多粗**」：这些量
/// 没有主题扩展的必要（它们不随亮暗切换），但**必须只有一份**。
///
/// 2026-08-21 清点过一次圆角：42 处现编的数字、14 个互不相同的值。间距与
/// 动效时长当时还没清点，但形状一样 —— `SizedBox(height: 7)`、`8`、`9`
/// 在同一个面板里并存，`Duration(milliseconds: 160)` 与 `150` 各写各的。
/// 单看每一处都说得过去，合起来就是「这个界面有点说不清哪里不齐」。
///
/// # 与 `CortexTokens` 的分工
///
/// * 颜色、圆角阶、投影色 → [`CortexTokens`]（主题扩展，随亮暗走）
/// * 间距、动效时长、字号**角色**、线宽 → 这里（常量，不随主题走）
///
/// 圆角**不在这里重新定义**：它已经有一份带语义的阶（窗口/卡片/输入框/
/// 行/胶囊），再抄一份就是两个真相。这里只做转发，让新代码只 import 一个
/// 文件也能拿到全套。
library;

import 'package:flutter/material.dart';

import '../core/motion.dart';
import '../core/theme.dart';

/// 间距阶。**4 的倍数**，只有 `sm = 6` 是刻意的例外。
///
/// # 为什么 6 不并进 4 或 8
///
/// 它是「同一个东西内部的两截」那一档：图标与它的文字、胶囊里的点与标签、
/// 状态点与标题。用 4 会挤成一坨，用 8 会让它读成两个独立的东西。
/// 这一档在参考产品里普遍存在（Tailwind 的 `1.5` = 6px），不是我们发明的。
///
/// # 为什么没有 10、14、18
///
/// 它们全都落在两档之间，而落在两档之间的数字**没有语义**：写的人只是
/// 「觉得 12 有点小、16 有点大」。一旦允许这种手感，清点时就再也分不出
/// 哪一处是刻意的例外、哪一处是随手写的。
abstract final class CortexSpace {
  /// 发丝线宽。**全产品只有这一个线宽**（焦点环的 1.5 是另一回事，见
  /// [CortexStroke.focus]）。
  static const double hair = 1;

  static const double xxs = 2;
  static const double xs = 4;
  static const double sm = 6;
  static const double md = 8;
  static const double lg = 12;
  static const double xl = 16;
  static const double xxl = 24;
  static const double xxxl = 32;

  /// 面板内边距。卡片那一档（不是页面那一档）。
  static const EdgeInsets panel = EdgeInsets.all(lg);

  /// 一行内容的内边距。侧栏会话行、工具行、菜单项共用。
  ///
  /// 左边比右边多 2：行尾通常挂着一个 26 宽的图标按钮，视觉重心要往左
  /// 找回来一点，否则标题看起来贴着左边、按钮却离右边很远。
  static const EdgeInsets row = EdgeInsets.fromLTRB(10, 6, 8, 6);

  /// 页面级留白。**只给整页**（设置页、画廊、项目墙），不给面板。
  static const EdgeInsets page = EdgeInsets.fromLTRB(xl, lg, xl, lg);
}

/// 动效时长与曲线。
///
/// # 为什么时长是常量，而不是「按距离算」
///
/// 这里的动效全是**状态切换**（悬停、展开、焦点环），不是位移转场。
/// 状态切换靠的是「快到来不及注意、慢到看得见」，与移动了多少像素无关。
/// 按距离算的那一套适合页面转场，而这里没有页面转场。
///
/// # 为什么统一走 [resolve]
///
/// 用户选了「减少动效」时，所有这些时长必须变成 0 —— **不是变短**。
/// 变短仍然是一次会动的位移，而位移正是前庭敏感最难受的一类。
/// 直接写 `Duration(milliseconds: 140)` 的调用点读不到那个开关；
/// 走这里读得到。
abstract final class CortexMotion {
  /// 按下/松手这种「手指底下」的反馈。
  static const Duration instant = Duration(milliseconds: 90);

  /// 悬停垫色、边框提亮。**最常用的一档。**
  static const Duration fast = Duration(milliseconds: 140);

  /// 展开/收起、焦点环。
  static const Duration base = Duration(milliseconds: 180);

  /// 浮层出现、抽屉。一屏只发生一次的那种。
  static const Duration slow = Duration(milliseconds: 260);

  /// 进场：快出慢入里的「慢入」，收尾稳。
  static const Curve enter = Curves.easeOutCubic;

  /// 退场：比进场快一点，离开不需要被看清。
  static const Curve exit = Curves.easeInCubic;

  /// 把时长交给「减少动效」开关裁决。
  static Duration resolve(BuildContext context, Duration duration) =>
      motionDuration(context, duration);
}

/// 线。**1px 是默认，也是唯一允许的常规线宽。**
///
/// 现代桌面 UI 的层级靠**色差 + 一条 1px 的线**，不靠粗边框。粗边框是
/// 表单控件时代的语言（「这里是一个输入框」），而这里的分隔是「两块内容
/// 之间的界」—— 界越细，内容越像浮在空间里。
abstract final class CortexStroke {
  /// 常态发丝线。实色，不用透明度：透明度造出来的线在每个表面上深浅
  /// 不一，而「每个表面上都是同一条线」才是发丝线语言的成立前提。
  static Border all(Color color) => Border.all(color: color, width: 1);

  /// 只有上边一条 —— 账号栏、面板底栏。
  static Border top(Color color) =>
      Border(top: BorderSide(color: color, width: 1));

  /// 只有下边一条 —— 面板头。
  static Border bottom(Color color) =>
      Border(bottom: BorderSide(color: color, width: 1));

  /// 焦点环。**1.5 是唯一一个不走 1px 的线宽**：它要与常态线区分开，
  /// 而焦点是「键盘此刻在这里」，是整屏唯一需要被一眼找到的东西。
  static Border focus(Color color) => Border.all(color: color, width: 1.5);
}

/// 投影。**只有真的浮起来的东西才用。**
///
/// 静态层级（卡片、面板、输入框）一律靠表面色阶 + 发丝线，不用阴影。
/// 阴影一多，整屏会发灰，而「浮起来」这个信号也随之失效。
abstract final class CortexElevation {
  /// 浮起：贴在内容上的浮条（钉底输入框、悬停抬起的卡片）。
  static List<BoxShadow> raised(BuildContext context) =>
      Theme.of(context).cortex.shadow2;

  /// 悬空：菜单、弹层、命令面板。
  static List<BoxShadow> popover(BuildContext context) =>
      Theme.of(context).cortex.shadow3;
}

/// 字号**角色**，不是字号表。
///
/// # 为什么要有这一层
///
/// `Theme.of(context).textTheme.bodySmall` 这个名字回答的是「它多大」，
/// 而写界面的人想的是「它是次要信息」。两者之间少一层翻译，结果就是
/// 每一处都自己 `copyWith(fontSize: 12.5)` —— 而 12.5、12、13 并存时，
/// 没有任何一处是错的，整屏却读起来不齐。
///
/// 这里只暴露**角色**：正文、次要、说明、区块标题、等宽。要新角色先想清楚
/// 它是不是上面某一个的别名，而不是再加一个字号。
abstract final class CortexType {
  /// 正文。13.5–14px、行高 1.6 —— 桌面端长文阅读的舒适区。
  static TextStyle body(BuildContext context) {
    final base = Theme.of(context).textTheme.bodyMedium!;
    return base.copyWith(height: 1.6);
  }

  /// 正文的强调版（用户消息、关键结论）。
  static TextStyle bodyStrong(BuildContext context) =>
      body(context).copyWith(fontWeight: FontWeight.w500);

  /// 次要信息：时间戳、模型名、路径、说明。
  static TextStyle secondary(BuildContext context) => body(context).copyWith(
    fontSize: 12.5,
    height: 1.5,
    color: Theme.of(context).colorScheme.onSurfaceVariant,
  );

  /// 说明/占位：第三级前景，**实色**。
  static TextStyle caption(BuildContext context) {
    final theme = Theme.of(context);
    return theme.textTheme.labelSmall!.copyWith(
      color: theme.cortex.foregroundTertiary,
    );
  }

  /// 区块小标题。15px / w600 / 负字距 —— Linear 一路的特征是
  /// **小而重**的标题，不是大而中庸。
  static TextStyle sectionTitle(BuildContext context) => Theme.of(context)
      .textTheme
      .titleMedium!
      .copyWith(fontSize: 15, fontWeight: FontWeight.w600, letterSpacing: -0.3);

  /// 等宽：代码、id、路径、工具名。
  static TextStyle mono(
    BuildContext context, {
    double size = 12,
    Color? color,
    FontWeight weight = FontWeight.w400,
  }) => TextStyle(
    fontFamily: 'JetBrains Mono',
    fontFamilyFallback: CortexTheme.monoFallback,
    fontSize: size,
    height: 1.55,
    fontWeight: weight,
    color: color ?? Theme.of(context).colorScheme.onSurfaceVariant,
  );
}

/// 圆角阶的转发 —— 让新代码只 import 一个文件就够。
///
/// 值仍然住在 [`CortexTokens`]：那里是唯一真相，这里只是别名。
abstract final class CortexRadius {
  static const double window = CortexTokens.radiusWindow;
  static const double card = CortexTokens.radiusCard;
  static const double input = CortexTokens.radiusInput;
  static const double row = CortexTokens.radiusRow;
  static const double pill = CortexTokens.radiusPill;
}
