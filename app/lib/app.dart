import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'core/motion.dart';
import 'core/theme.dart';
import 'features/auth/login_gate.dart';
import 'state/app_providers.dart';

class CortexApp extends ConsumerWidget {
  const CortexApp({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    // **在外层 watch，不要在 `builder` 闭包里 watch。** 那个闭包是在
    // 一个后代 context 里、可能在另一帧被调的，在里面 `ref.watch`
    // 注册的依赖挂在这个 widget 上却不由它的 build 驱动
    final motion = ref.watch(motionPrefProvider);

    return MaterialApp(
      title: 'Cortex',
      debugShowCheckedModeBanner: false,
      theme: CortexTheme.light(),
      darkTheme: CortexTheme.dark(),
      // 存在 settings.json 里，见 ThemeModeNotifier；顶栏那个按钮循环切
      themeMode: ref.watch(themeModeProvider),
      // 动效三档在这里落地：**改写 `MediaQuery.disableAnimations` 本身**，
      // 而不是另发明一个只有我们自己读的标志。
      //
      // 这样整棵树（包括 Flutter 自己的路由转场、`Scrollable` 的隐式滚动、
      // 以及所有 `reducedMotion(context)` 的调用点）读到的是**同一位**。
      // 另起一套的下场是：用户选了「关闭」，我们的三个点停了，
      // 框架的转场照旧 —— 一个只兑现一半的开关。
      builder: (context, child) {
        final media = MediaQuery.of(context);
        return MediaQuery(
          data: media.copyWith(
            disableAnimations: resolveDisableAnimations(
              motion,
              system: media.disableAnimations,
            ),
          ),
          // 没有路由在建时 child 为 null（`builder` 的约定）
          child: child ?? const SizedBox.shrink(),
        );
      },
      // The gate, not the shell: a real `cortexd` refuses to start without
      // credentials configured, so "can we even talk to it" has to be answered
      // before any pane is worth rendering.
      home: const LoginGate(),
      scrollBehavior: const _DesktopScrollBehavior(),
    );
  }
}

/// Enables mouse/trackpad drag-scrolling, which Flutter disables by default on
/// desktop and web. Without it, dragging inside the transcript does nothing.
class _DesktopScrollBehavior extends MaterialScrollBehavior {
  const _DesktopScrollBehavior();

  @override
  Set<PointerDeviceKind> get dragDevices => const {
    PointerDeviceKind.touch,
    PointerDeviceKind.mouse,
    PointerDeviceKind.trackpad,
    PointerDeviceKind.stylus,
  };
}
