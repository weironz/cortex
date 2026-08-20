/// 系统里的「减少动效」真的被听进去了吗。
///
/// # 为什么这条测试值得存在
///
/// 这个产品里的动效**几乎全是循环的**：等待回复的三个点、流式输出的光标、
/// 代码块那个进度点。循环动画正是 `prefers-reduced-motion` 最主要的目标 ——
/// 它不是「好看一点」，对前庭功能敏感的人是会引起不适的东西。
///
/// 而这件事**永远不会有人手动发现**：开发机上没人开那个系统开关，
/// 开了它的用户也不会来报「你们没关动画」，他只会觉得这个应用用着难受。
library;

import 'package:cortex_app/core/motion.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 一个只做「循环」这件事的最小部件，用来单独测 [syncLoop]。
class _Looping extends StatefulWidget {
  const _Looping({required this.onController});
  final void Function(AnimationController) onController;

  @override
  State<_Looping> createState() => _LoopingState();
}

class _LoopingState extends State<_Looping>
    with SingleTickerProviderStateMixin {
  late final AnimationController _c = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 500),
  );

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    syncLoop(_c, context);
    widget.onController(_c);
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => const SizedBox.shrink();
}

Future<AnimationController> _pump(
  WidgetTester tester, {
  required bool disableAnimations,
}) async {
  late AnimationController captured;
  await tester.pumpWidget(
    MediaQuery(
      data: MediaQueryData(disableAnimations: disableAnimations),
      child: Directionality(
        textDirection: TextDirection.ltr,
        child: _Looping(onController: (c) => captured = c),
      ),
    ),
  );
  return captured;
}

void main() {
  testWidgets('没开减少动效时照常循环', (tester) async {
    final c = await _pump(tester, disableAnimations: false);
    expect(c.isAnimating, isTrue, reason: '默认就该动 —— 这条红了说明把所有人的动效都关掉了');
    // 收尾：让 pumpAndSettle 不至于等一个永不停止的循环
    c.stop();
  });

  testWidgets('开了减少动效就停下，并且停在**看得见**的那一端', (tester) async {
    final c = await _pump(tester, disableAnimations: true);
    expect(
      c.isAnimating,
      isFalse,
      reason:
          '循环动画是 prefers-reduced-motion 最主要的目标。'
          '不停的话，对前庭功能敏感的人会一直难受，而他不会来报这个',
    );
    expect(
      c.value,
      1.0,
      reason:
          '停在 0 的话光标与那三个点会**消失** —— 于是「减少动效」变成了'
          '「这个东西不见了」。停在终态，让它是一个静止的、存在的东西',
    );
  });

  testWidgets('用户在系统里改了之后当场生效，不用重启', (tester) async {
    final c = await _pump(tester, disableAnimations: false);
    expect(c.isAnimating, isTrue);

    // 同一棵树，只换 MediaQuery —— 模拟用户去系统设置里打开了那个开关
    await tester.pumpWidget(
      MediaQuery(
        data: const MediaQueryData(disableAnimations: true),
        child: Directionality(
          textDirection: TextDirection.ltr,
          child: _Looping(onController: (_) {}),
        ),
      ),
    );
    expect(
      c.isAnimating,
      isFalse,
      reason:
          '判据写在 didChangeDependencies 而不是 initState，就是为了这一条：'
          '写在 initState 里只有重启应用才生效',
    );
  });
}
