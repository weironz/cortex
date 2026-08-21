import 'package:flutter/material.dart';

/// 用户对动效的表态。**默认跟随系统**，另外两档是显式覆盖。
///
/// # 为什么不照抄 LobeHub 的 `agile / default / disabled`
///
/// 它那三档调的是**转场时长**（`motionUnit`），而这个产品里的动效
/// 几乎全是**循环指示器**（等待回复的三个点、流式光标、代码块的进度点），
/// 只有两处是真转场。把一个循环指示器调快，得到的是一个转得更急、
/// 更抢注意力的角落 —— 与 `agile` 承诺的「轻快」正好相反。
///
/// 所以这里三档调的是**要不要**，不是**多快**：真正没被满足的需求是
/// 「我想表达一个和系统不一样的意见」，而那需要**两个方向都能覆盖** ——
/// 系统开着但我在这个应用里想要动效，以及系统没开但我不想要。
/// 只做「关闭」那一档会漏掉前一半。
enum MotionPref {
  /// 跟着 `MediaQuery.disableAnimations` 走。
  system('system'),

  /// 要动效，**即使系统说减少**。
  full('full'),

  /// 不要动效，**即使系统没开那个开关**。
  reduced('reduced');

  const MotionPref(this.wire);

  /// 存进 settings.json 的字符串。用显式常量而不是 `name`：
  /// 改个变体名不该让所有人的设置静默回到默认档。
  final String wire;

  /// 认不出来就回 [system]。
  ///
  /// 读坏的配置**必须落在「跟随系统」上**，不能落在 `full` ——
  /// 那等于拿一个损坏的文件去推翻用户在系统里表达过的无障碍偏好。
  static MotionPref fromWire(String? raw) {
    for (final p in values) {
      if (p.wire == raw) return p;
    }
    return MotionPref.system;
  }
}

/// 三档 + 系统那一位 → 最终「要不要禁用动效」。
///
/// 单独抽出来是为了能直接测：它是这个功能里唯一有分支的地方，
/// 而挂在 widget 树上测同一件事要起一整个 `MaterialApp`。
bool resolveDisableAnimations(MotionPref pref, {required bool system}) =>
    switch (pref) {
      MotionPref.system => system,
      MotionPref.full => false,
      MotionPref.reduced => true,
    };

/// 现在该不该省掉动效。
///
/// # 为什么必须有这么一个开关
///
/// 这个产品里的动效**几乎全是循环的**（等待回复的三个点、流式输出的光标、
/// 代码块复制后的提示）。循环动画正是 `prefers-reduced-motion` 最主要的
/// 目标：它不是「好看一点」，对前庭功能敏感的人是**会引起不适**的东西，
/// 对注意力障碍的人是一个永远在动、永远在抢注意力的角落。
///
/// # 为什么读的还是 `MediaQuery`，而不是那个 provider
///
/// [MotionPref] 的三档在 `app.dart` 里就**改写进了 `MediaQuery`**
/// （`copyWith(disableAnimations:)`），所以这里一个字都不用改，
/// 而好处是两条：
///
/// 1. 调用点大多在 `State.didChangeDependencies` 里，那里没有 `ref`；
///    要它们全变成 `ConsumerState` 只为读一个偏好，代价与收益不成比例。
/// 2. **Flutter 框架自己也读这一位**（路由转场、`Scrollable` 的隐式滚动）。
///    另起一套判据的话，用户选了「关闭」，我们自己的三个点停了，
///    而框架的转场照旧 —— 一个只兑现一半的开关。
bool reducedMotion(BuildContext context) =>
    MediaQuery.maybeDisableAnimationsOf(context) ?? false;

/// 一个转场该用多久。关掉动效时是 [Duration.zero]（**不是「短一点」**：
/// 那仍然是一次会动的位移）。
Duration motionDuration(BuildContext context, Duration full) =>
    reducedMotion(context) ? Duration.zero : full;

/// 让一个**循环**动画跟着「减少动效」走。
///
/// 在 `didChangeDependencies` 里调：那里 `MediaQuery` 已经可用，
/// 而且用户在系统设置里改了之后会**再调一次**——写在 `initState` 里的话，
/// 只有重启应用才会生效。
///
/// [restingValue] 是关掉动效之后停在哪。**不要停在 0**：
/// 三个点的透明度公式在 0 处最淡、光标在 0 处几乎看不见，
/// 于是「减少动效」会变成「这个东西不见了」。停在终态，
/// 让它看起来是一个**静止的、存在的**东西。
void syncLoop(
  AnimationController controller,
  BuildContext context, {
  bool reverse = false,
  double restingValue = 1,
}) {
  if (reducedMotion(context)) {
    controller.stop();
    controller.value = restingValue;
    return;
  }
  if (!controller.isAnimating) controller.repeat(reverse: reverse);
}
