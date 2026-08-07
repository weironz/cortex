import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api_exception.dart';
import '../api/cortex_api.dart';
import '../api/http_cortex_api.dart';
import '../auth/token_store.dart';
import '../models/health_status.dart';
import 'app_providers.dart';

/// Where the client stands with respect to `cortexd`'s front door.
enum AuthPhase {
  /// `GET /health` is in flight. Nothing is known yet, including whether a
  /// credential will be wanted.
  probing,

  /// The daemon could not be reached at all. Distinct from [needsToken]
  /// because the remedy is different — start the daemon or fix the address,
  /// not find a token — and because showing a password field to someone whose
  /// server is down is a small cruelty.
  unreachable,

  /// The daemon checks credentials and we do not have an accepted one.
  needsToken,

  /// A credential was presented and accepted, or the daemon does not check.
  ready,
}

class AuthState {
  const AuthState({
    this.phase = AuthPhase.probing,
    this.token,
    this.health,
    this.error,
    this.busy = false,
    this.remember = false,
  });

  final AuthPhase phase;

  /// The accepted credential, or null when none is held.
  ///
  /// Never rendered. [toString] is overridden below because a `Notifier`'s
  /// state ends up in Riverpod observers and error messages, and "the token
  /// leaked through a debug print" is a failure mode with no symptom.
  final String? token;

  /// Last `/health` reading. Drives the "this deployment has authentication
  /// turned off" notice as well as the version line.
  final HealthStatus? health;

  /// Why the last attempt failed, in the daemon's own words where it had any.
  final String? error;

  /// A sign-in or probe is running.
  final bool busy;

  /// Whether the user asked the platform to keep the token. Only meaningful
  /// where [kCanRememberToken] is true.
  final bool remember;

  bool get isReady => phase == AuthPhase.ready;

  /// True when the daemon told us it accepts anybody who can reach the port.
  bool get serverAuthDisabled => health?.authDisabled ?? false;

  AuthState copyWith({
    AuthPhase? phase,
    Object? token = _sentinel,
    Object? health = _sentinel,
    Object? error = _sentinel,
    bool? busy,
    bool? remember,
  }) => AuthState(
    phase: phase ?? this.phase,
    token: token == _sentinel ? this.token : token as String?,
    health: health == _sentinel ? this.health : health as HealthStatus?,
    error: error == _sentinel ? this.error : error as String?,
    busy: busy ?? this.busy,
    remember: remember ?? this.remember,
  );

  static const Object _sentinel = Object();

  /// Redacted on purpose — see [token].
  @override
  String toString() =>
      'AuthState(phase: $phase, token: ${token == null ? 'none' : '<redacted>'},'
      ' busy: $busy)';
}

/// Owns the credential and the decision about whether one is needed.
///
/// ## Why the gate exists at all
///
/// `cortexd` refuses to start without credentials configured, so **every real
/// deployment checks them**. A client with no way to present one cannot talk to
/// any real server — it would show an empty session list and a `DOWN` badge,
/// with nothing anywhere explaining that the problem is a missing token.
///
/// ## Why `/health` decides, not a config flag
///
/// The alternative is a "this server needs a token" checkbox, which asks the
/// user a question the server can answer. `/health` is unauthenticated
/// precisely so it can be asked before anyone has a credential, and it reports
/// `auth: "token" | "disabled"`. So a loopback dev daemon running
/// `CORTEX_AUTH=disabled` lets the user straight through, and a real one asks —
/// with no setting to get wrong in either direction.
class AuthController extends Notifier<AuthState> {
  /// Guards against a stale probe (from a previous base URL) writing its result
  /// over a newer one.
  int _generation = 0;

  @override
  AuthState build() {
    // A different daemon means a different credential: the token that opens one
    // is meaningless at the other, and silently carrying it over would produce
    // a 401 the user cannot connect to their own address change.
    ref.listen(appConfigProvider, (previous, next) {
      if (previous?.baseUrl != next.baseUrl || previous?.useMock != next.useMock) {
        _reset();
      }
    });
    Future.microtask(probe);
    return _seeded();
  }

  /// Initial state, with whatever the platform can supply unattended.
  ///
  /// The seed is read here rather than at sign-in so that a token supplied by
  /// the environment (desktop) or kept for the tab (web) is already in hand
  /// when the probe discovers that one is required — that combination is what
  /// makes a correctly configured setup skip the login screen entirely.
  AuthState _seeded() {
    final seed = ref.read(authSeedTokenProvider);
    return AuthState(token: seed, remember: seed != null);
  }

  /// Asks the unauthenticated `/health`, then decides.
  Future<void> probe() async {
    final generation = ++_generation;
    if (!ref.mounted) return;

    // The mock source has no port and no daemon; probing it over HTTP would
    // hit whatever happens to be listening on the configured address. Short-
    // circuited here rather than by asking `cortexApiProvider` for the mock
    // instance, because that provider reads *this* controller for the token —
    // going the other way would close a dependency cycle.
    if (ref.read(appConfigProvider).useMock) {
      state = state.copyWith(
        phase: AuthPhase.ready,
        health: null,
        busy: false,
        error: null,
      );
      return;
    }

    state = state.copyWith(busy: true, error: null);

    final probeApi = _probeApi();
    try {
      final health = await probeApi.health();
      if (!_alive(generation)) return;

      if (!health.requiresToken) {
        // Includes the mock source, which reports `disabled` because there is
        // no port to protect.
        state = state.copyWith(
          phase: AuthPhase.ready,
          health: health,
          busy: false,
          error: null,
        );
        return;
      }
      // A token in hand still has to be *accepted*; an expired or rotated one
      // looks identical to a good one until something authenticated is tried.
      final held = state.token;
      state = state.copyWith(health: health, busy: false);
      if (held != null && held.isNotEmpty) {
        await signIn(held, remember: state.remember);
      } else {
        state = state.copyWith(phase: AuthPhase.needsToken, busy: false);
      }
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(
        phase: AuthPhase.unreachable,
        busy: false,
        error: e.message,
      );
    } on Object catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(
        phase: AuthPhase.unreachable,
        busy: false,
        error: '$e',
      );
    } finally {
      probeApi.dispose();
    }
  }

  /// Validates [rawToken] and, if the daemon accepts it, opens the app.
  ///
  /// ## Why `POST /auth/ticket` is the probe
  ///
  /// It is authenticated (so it answers the question), it is tiny, and it has
  /// no side effect beyond minting a credential that expires in a minute. The
  /// obvious alternative — `GET /sessions` — would work but pulls a page of
  /// real data purely to test a header, and it fails for reasons unrelated to
  /// authentication (an empty database, a storage hiccup), which would surface
  /// to the user as "wrong token".
  Future<void> signIn(String rawToken, {bool remember = false}) async {
    final token = rawToken.trim();
    if (token.isEmpty) return;
    final generation = ++_generation;
    state = state.copyWith(busy: true, error: null);

    final probeApi = _probeApi(token: token);
    try {
      await probeApi.issueTicket();
      if (!_alive(generation)) return;

      if (remember && kCanRememberToken) {
        await rememberToken(token);
      } else if (!remember && kCanRememberToken) {
        await forgetToken();
      }
      if (!_alive(generation)) return;
      state = state.copyWith(
        phase: AuthPhase.ready,
        token: token,
        busy: false,
        error: null,
        remember: remember,
      );
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(
        // An unreachable daemon is not a rejected token. Conflating them puts
        // the user in a loop of retyping a credential that was never the
        // problem.
        phase: e.isUnauthorized ? AuthPhase.needsToken : AuthPhase.unreachable,
        busy: false,
        error: e.isUnauthorized
            ? 'cortexd 拒绝了这个 token。它区分不出「没带」与「带错了」——'
                  '分开报等于给暴力破解一个进度条 —— 所以这里也只能说：没通过。'
                  '确认复制的是 `cortexd --generate-token` 输出的 CORTEXD_TOKEN 明文，'
                  '而不是写进服务端 .env 的那串摘要。'
            : e.message,
        token: null,
      );
    } on Object catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(
        phase: AuthPhase.unreachable,
        busy: false,
        error: '$e',
      );
    } finally {
      probeApi.dispose();
    }
  }

  /// The credential stopped working mid-session.
  ///
  /// Called from any 401, from anywhere, via `HttpCortexApi.onUnauthorized`.
  /// The whole app drops back to the gate rather than leaving panes that
  /// silently fail to load — which is the shape the same failure takes if
  /// nobody handles it.
  void onUnauthorized() {
    if (state.phase == AuthPhase.needsToken) return; // already there
    // Deliberately not `forgetToken()`: a rotated server-side secret is not a
    // reason to also destroy the copy the user may still be editing, and on
    // desktop the "stored" copy is an environment variable this app must not
    // pretend it can unset.
    state = state.copyWith(
      phase: AuthPhase.needsToken,
      token: null,
      error: '凭据已失效或被拒绝（HTTP 401）。请重新填写 token。',
    );
  }

  /// Explicit sign-out. Drops the in-memory credential and any stored copy.
  Future<void> signOut() async {
    if (kCanRememberToken) await forgetToken();
    if (!ref.mounted) return;
    state = state.copyWith(
      phase: AuthPhase.needsToken,
      token: null,
      error: null,
      remember: false,
    );
  }

  void setRemember(bool value) {
    if (state.remember == value) return;
    state = state.copyWith(remember: value);
  }

  void _reset() {
    ++_generation;
    state = _seeded();
    Future.microtask(probe);
  }

  bool _alive(int generation) => ref.mounted && generation == _generation;

  CortexApi _probeApi({String? token}) =>
      ref.read(authProbeApiProvider)(token);
}

/// The credential the platform can hand over with no user action.
///
/// Desktop reads `CORTEXD_TOKEN` from the environment; Web reads the copy the
/// user opted into keeping for the tab. A provider rather than a direct call so
/// that the tests can pin it — otherwise every case about the login screen
/// would quietly change behaviour on a developer machine that has the variable
/// set, which is precisely the machine we tell people to have.
final authSeedTokenProvider = Provider<String?>((ref) => readSeedToken());

/// Builds the throwaway client the gate probes with.
///
/// A separate provider from [cortexApiProvider] for two reasons, one structural
/// and one practical:
///
/// * **Structural** — that provider's client is built *from* the credential
///   this controller is trying to validate. Using it here would make the gate
///   depend on the thing it gates, and every rejected sign-in attempt would
///   tear down and rebuild the entire data layer.
/// * **Practical** — it is the seam the tests substitute, which is what lets
///   the gate's state machine be exercised without a socket.
///
/// The instances it returns carry no `onUnauthorized` hook: a 401 *here* is the
/// answer to the question being asked, not an event to broadcast, and feeding
/// it back into [AuthController.onUnauthorized] would overwrite the specific
/// message [AuthController.signIn] is about to write with a generic one.
final authProbeApiProvider = Provider<CortexApi Function(String? token)>((ref) {
  final baseUrl = ref.watch(appConfigProvider).baseUrl;
  return (token) => HttpCortexApi(baseUrl: baseUrl, token: token);
});

final authControllerProvider = NotifierProvider<AuthController, AuthState>(
  AuthController.new,
);
