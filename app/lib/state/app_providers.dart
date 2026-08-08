/// ## Why Riverpod
///
/// Three things decided it over Provider and Bloc:
///
/// 1. **The mock/live swap is the app's central seam.** With Riverpod the
///    entire data source is one `Provider` whose body reads config; every
///    consumer is invalidated automatically when it flips. With `Bloc` the same
///    swap means re-wiring repositories through constructors of a dozen blocs.
/// 2. **`select` gives bubble-level rebuild isolation for free.** Streaming
///    text updates ~60×/s; the conversation list must not rebuild at that rate.
///    `ref.watch(p.select((s) => s.streamingText))` confines the rebuild to the
///    one widget that reads it — the same thing with `ChangeNotifierProvider`
///    needs hand-rolled `Selector`s, and with Bloc needs `buildWhen` on every
///    `BlocBuilder`.
/// 3. **No `BuildContext` for reads.** Controllers can react to each other
///    (session switch → cancel in-flight stream) without a widget in scope.
///
/// Cost accepted: `NotifierProvider` boilerplate is wordier than
/// `ChangeNotifier`, and Riverpod 3's generics show up in error messages. Both
/// are one-time costs; the rebuild isolation is a per-frame benefit.
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/cortex_api.dart';
import '../api/http_cortex_api.dart';
import '../api/mock_cortex_api.dart';
import '../core/app_config.dart';
import '../core/local_agent.dart';
import '../models/health_status.dart';
import 'auth_controller.dart';

/// Mutable runtime config. Seeded from `--dart-define`, editable in settings.
class AppConfigNotifier extends Notifier<AppConfig> {
  @override
  AppConfig build() => AppConfig.initial;

  void setUseMock(bool value) {
    if (state.useMock == value) return;
    state = state.copyWith(useMock: value);
  }

  void setBaseUrl(String value) {
    final trimmed = value.trim();
    if (trimmed.isEmpty || state.baseUrl == trimmed) return;
    state = state.copyWith(baseUrl: trimmed);
  }
}

final appConfigProvider = NotifierProvider<AppConfigNotifier, AppConfig>(
  AppConfigNotifier.new,
);

/// The bundled agent's origin, or null when this build runs without one.
///
/// ## Why it starts only after sign-in
///
/// The agent needs the remote URL *and* the token — it proxies to `cortexd` on
/// the app's behalf. Starting it earlier would mean holding a credential the
/// user has not yet proven is valid.
///
/// The token check itself must hit the **remote**, which is why
/// [AuthController.signIn] probes `config.baseUrl` rather than the agent: the
/// agent validates inbound requests against the very token the app handed it,
/// so asking it "is this token good?" proves nothing.
///
/// ## null is a normal outcome, not an error
///
/// No binary beside the app (a `flutter run` build), Web, or an agent that
/// refuses to start — all fall back to talking to the remote directly. That
/// costs local tools, not the app. Failing sign-in over it would turn a
/// degraded feature into a total outage.
///
/// ## Disposal kills it
///
/// Signing out or switching backends disposes this provider, which stops the
/// agent. That matters: it holds the token and can execute commands, so it must
/// not outlive the session that authorised it.
final localAgentOriginProvider = FutureProvider<String?>((ref) async {
  final config = ref.watch(appConfigProvider);
  final token = ref.watch(authControllerProvider.select((s) => s.token));
  if (config.useMock || token == null || !kLocalAgentSupported) return null;

  final agent = discoverLocalAgent();
  if (agent == null) return null;
  ref.onDispose(() => unawaited(agent.stop()));

  try {
    final origin = await agent.start(remote: config.baseUrl, token: token);
    debugPrint('本地 agent 已就绪：$origin（工具在本机执行）');
    return origin;
  } on LocalAgentException catch (e) {
    // Loud in the log, silent in the UI: the app still works, it just runs
    // tools on the server. Blocking here would be worse than degrading.
    debugPrint('本地 agent 未启动，回落到直连远端：$e');
    return null;
  }
});

/// The single place that knows whether we are on mock or live data.
///
/// Rebuilt whenever [appConfigProvider] changes, which cascades an invalidation
/// through every dependent provider — sessions, memory search, health — so the
/// UI fully re-hydrates against the new backend without manual refresh calls.
///
/// ## Why the credential is watched here too
///
/// The token is baked into the client at construction rather than read per
/// request, so it cannot drift mid-flight. Signing in or out is
/// therefore the same kind of event as switching backends — a different
/// identity is a different data source — and it re-hydrates by exactly the same
/// mechanism instead of needing its own refresh path.
///
/// Only the *token* is watched, not the whole [AuthState]: `select` keeps a
/// countdown tick or a busy flag from tearing down every in-flight request.
final cortexApiProvider = Provider<CortexApi>((ref) {
  final config = ref.watch(appConfigProvider);
  if (config.useMock) {
    final api = MockCortexApi();
    ref.onDispose(api.dispose);
    return api;
  }

  final token = ref.watch(authControllerProvider.select((s) => s.token));

  // Point at the local agent once it is up; the remote until then, and forever
  // if this build has none. Both speak the same protocol — the agent
  // reverse-proxies everything it does not handle — so nothing downstream can
  // tell the difference, and the swap is just another backend change.
  //
  // `config.baseUrl` stays the *remote* everywhere it is displayed. That is
  // what the user configured and what they care about; showing them
  // `127.0.0.1:51234` would be true and useless.
  final origin =
      ref.watch(localAgentOriginProvider).value ?? config.baseUrl;

  final api = HttpCortexApi(
    baseUrl: origin,
    token: token,
    // `read`, not `watch`: this is an outbound edge. Watching the notifier
    // would make every auth state change rebuild the client, including the one
    // this very callback triggers.
    onUnauthorized: () =>
        ref.read(authControllerProvider.notifier).onUnauthorized(),
  );
  ref.onDispose(api.dispose);
  return api;
});

/// `GET /health`, polled lazily (on demand + on manual refresh).
final healthProvider = FutureProvider<HealthStatus>((ref) async {
  final api = ref.watch(cortexApiProvider);
  return api.health();
});

/// System-following theme mode with a manual override.
class ThemeModeNotifier extends Notifier<ThemeMode> {
  @override
  ThemeMode build() => ThemeMode.system;

  void cycle() {
    state = switch (state) {
      ThemeMode.system => ThemeMode.light,
      ThemeMode.light => ThemeMode.dark,
      ThemeMode.dark => ThemeMode.system,
    };
  }
}

final themeModeProvider = NotifierProvider<ThemeModeNotifier, ThemeMode>(
  ThemeModeNotifier.new,
);
