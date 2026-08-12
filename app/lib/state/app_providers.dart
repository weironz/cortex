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
/// How many consecutive deaths before we stop trying.
///
/// Some failures never get better by retrying — a protocol mismatch with the
/// remote, a corrupt binary, a port the OS refuses. Restarting those forever
/// burns CPU and buries the real error under a wall of identical log lines.
/// Give up, stay on the remote, and leave the reason in the log.
const _maxConsecutiveRestarts = 5;

/// Live this long and the run counts as healthy — the failure budget resets.
///
/// Without this, an agent that dies once a day would eventually exhaust a
/// lifetime budget and stop being restarted, which is the opposite of what a
/// supervisor is for. What we are actually trying to detect is a *crash loop*,
/// and a crash loop is defined by dying **quickly**, not by dying often.
const _healthyRunThreshold = Duration(seconds: 30);

/// Survives [localAgentOriginProvider] being invalidated, which is exactly why
/// it is separate: the restart counter has to outlive the thing it counts.
///
/// It does **not** survive signing in again or pointing at a different server.
/// Both are the user actively changing the situation, and both are the actual
/// fix for the failures that exhaust this budget — a protocol mismatch is
/// resolved by aiming at a different daemon, and a stale token by signing in.
/// Keeping a spent budget across either would mean the fix appears not to work.
final _agentRestartBudgetProvider = Provider<_RestartBudget>((ref) {
  ref.watch(authControllerProvider.select((s) => s.token));
  ref.watch(appConfigProvider.select((c) => c.baseUrl));
  return _RestartBudget();
});

class _RestartBudget {
  int consecutive = 0;

  bool get exhausted => consecutive >= _maxConsecutiveRestarts;

  /// 1s, 2s, 4s, 8s, 16s. Backing off matters more than it looks: the common
  /// cause of an instant re-death is something transient holding a resource,
  /// and hammering it is how a transient failure becomes a permanent one.
  Duration get delay => Duration(seconds: 1 << consecutive.clamp(0, 4));
}

final localAgentOriginProvider = FutureProvider<String?>((ref) async {
  final config = ref.watch(appConfigProvider);
  final token = ref.watch(authControllerProvider.select((s) => s.token));
  // 关掉认证的部署（`CORTEX_AUTH=disabled`，自托管跑在 127.0.0.1 上的
  // 常见形态）根本没有 token 可拿。
  //
  // **原先这里只看 token 是否为 null**，于是那种部署会：跳过登录界面
  // （对的）、然后**静默地不启动本地 agent** —— 用户看到的是「装了桌面端，
  // 但它读不到我本机的文件」，而界面上没有任何一处说明为什么。
  // 两件事本来就没有因果关系：本地 agent 要的是「能连上 cortexd」，
  // 不是「有一把 token」。
  final needsToken = ref.watch(
    authControllerProvider.select((s) => s.health?.requiresToken ?? true),
  );
  if (config.useMock || !kLocalAgentSupported) return null;
  if (needsToken && token == null) return null;

  final agent = discoverLocalAgent();
  if (agent == null) return null;

  var disposed = false;
  Timer? restartTimer;
  ref.onDispose(() {
    disposed = true;
    restartTimer?.cancel();
    unawaited(agent.stop());
  });

  final budget = ref.read(_agentRestartBudgetProvider);
  if (budget.exhausted) {
    debugPrint(
      '本地 agent 连续 $_maxConsecutiveRestarts 次启动即退出，不再重试；'
      '已回落到直连远端。重新登录可重置',
    );
    return null;
  }

  final startedAt = DateTime.now();

  /// One policy for both ways this can fail.
  ///
  /// "It died after running" and "it never came up" look different but call for
  /// the same response: back off, try again, and stop after enough tries. Two
  /// separate policies would drift — and the one that got forgotten would be
  /// the silent one.
  void scheduleRestart(String why) {
    if (disposed) return;

    // A run that lasted counts as proof the thing works; whatever killed it was
    // not a startup failure. Reset before incrementing so a long-lived agent
    // that dies once starts over at a 1-second backoff.
    if (DateTime.now().difference(startedAt) >= _healthyRunThreshold) {
      budget.consecutive = 0;
    }
    budget.consecutive++;
    debugPrint('本地 agent（第 ${budget.consecutive} 次）：$why');

    if (budget.exhausted) {
      debugPrint('不再重试本地 agent —— 上面那段是它最后说的话。已回落到直连远端');
      // Still invalidate: the provider must re-run to hand the rest of the app
      // a null origin, otherwise every request keeps going to a port nobody is
      // listening on.
      ref.invalidateSelf();
      return;
    }
    restartTimer = Timer(budget.delay, () {
      if (!disposed) ref.invalidateSelf();
    });
  }

  try {
    final origin = await agent.start(
      remote: config.baseUrl,
      // 关掉认证的部署没有 token。空串而不是抛错：本地 agent 那侧
      // 也只是把它塞进 Authorization 头，而一个不认证的 cortexd 根本不看
      token: token ?? '',
      // Unexpected death only — a deliberate `stop()` never lands here, so
      // signing out cannot be mistaken for a crash.
      //
      // The tail is the only record of *why*: `cortex-local` refusing to start
      // on a protocol mismatch prints one line, and without it the user sees
      // nothing but "tools stopped working".
      onExit: (code, logTail) => scheduleRestart(
        '退出，code=$code${logTail.isEmpty ? '' : '\n$logTail'}',
      ),
    );
    debugPrint('本地 agent 已就绪：$origin（工具在本机执行）');
    return origin;
  } on LocalAgentException catch (e) {
    // Loud in the log, silent in the UI: the app still works, it just runs
    // tools on the server. Blocking here would be worse than degrading.
    //
    // Retried on the same budget rather than given up on: the usual cause of a
    // one-off start failure is something transient holding the executable (a
    // virus scanner sweeping a freshly-installed .exe is the common one), and
    // that clears on its own within seconds. A permanent cause — a protocol
    // mismatch, a missing binary — burns the budget instead and then stops.
    scheduleRestart('$e');
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
