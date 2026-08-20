import 'package:flutter/material.dart';

/// 系统里那个「减少动效」开关开着吗。
///
/// # 为什么必须读它
///
/// 这个产品里的动效**几乎全是循环的**（等待回复的三个点、流式输出的光标、
/// 代码块复制后的提示）。循环动画正是 `prefers-reduced-motion` 最主要的
/// 目标：它不是「好看一点」，对前庭功能敏感的人是**会引起不适**的东西，
/// 对注意力障碍的人是一个永远在动、永远在抢注意力的角落。
///
/// Cherry Studio 的设计契约里写了这一条（「Motion should … respect
/// `prefers-reduced-motion`」），LobeHub 干脆把它做成了用户可选的三档
/// （`agile / default / disabled`）。我们此前**一档都没读**。
///
/// # 判据取自系统，不是我们自己的设置
///
/// Windows 的「显示动画效果」、macOS 的「减弱动态效果」、浏览器的
/// `prefers-reduced-motion` —— Flutter 把它们统一成
/// `MediaQuery.disableAnimations`。用户在系统里表达过一次的偏好，
/// 不该要求他在每个应用里再表达一遍。
bool reducedMotion(BuildContext context) =>
    MediaQuery.maybeDisableAnimationsOf(context) ?? false;

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
