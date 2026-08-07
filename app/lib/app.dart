import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'core/theme.dart';
import 'features/auth/login_gate.dart';
import 'state/app_providers.dart';

class CortexApp extends ConsumerWidget {
  const CortexApp({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return MaterialApp(
      title: 'Cortex',
      debugShowCheckedModeBanner: false,
      theme: CortexTheme.light(),
      darkTheme: CortexTheme.dark(),
      // Defaults to ThemeMode.system; the header button cycles the override.
      themeMode: ref.watch(themeModeProvider),
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
