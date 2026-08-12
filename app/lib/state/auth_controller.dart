import 'dart:async';

import 'package:flutter/foundation.dart';
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
    this.refreshToken,
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

  /// 长效凭据。**只有它值得存**，而且只存在系统凭据库里。
  ///
  /// 留在 state 里是为了两件事：续期时拿它去换，以及登出时告诉服务端
  /// 「作废这条链」—— 后者是本地删掉一份副本做不到的。
  ///
  /// 同样被 [toString] 抹掉。
  final String? refreshToken;

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
    Object? refreshToken = _sentinel,
  }) => AuthState(
    phase: phase ?? this.phase,
    token: token == _sentinel ? this.token : token as String?,
    health: health == _sentinel ? this.health : health as HealthStatus?,
    error: error == _sentinel ? this.error : error as String?,
    busy: busy ?? this.busy,
    remember: remember ?? this.remember,
    refreshToken: refreshToken == _sentinel
        ? this.refreshToken
        : refreshToken as String?,
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
      if (previous?.baseUrl != next.baseUrl ||
          previous?.useMock != next.useMock ||
          previous?.offline != next.offline) {
        _reset();
      }
    });
    Future.microtask(_bootstrap);
    return _seeded();
  }

  /// 启动时先**把上次的登录续上**，续不上再走探测。
  ///
  /// # 这一步曾经完全不存在
  ///
  /// 登录时把 refresh token 存进了系统凭据库（`rememberToken`），
  /// 而启动时**从来没有读回来** —— `restoreSession` 写好了、测过了，
  /// 全项目零调用。于是「登录一次管 30 天」这件事在产品里是不存在的：
  /// 每次重启都回到登录页，桌面端与 Web 都是。
  ///
  /// 读取是异步的（要跨进程问钥匙串），而 `build` 是同步的，
  /// 所以只能排到微任务里 —— 这也是当初漏掉它的原因：`_seeded()` 是同步的，
  /// 只拿得到环境变量那一份，看起来「已经在读了」。
  Future<void> _bootstrap() async {
    if (!ref.mounted) return;
    // 记下开工时的代次。**中途有别人接手就整个让位** ——
    // 这一段全是 await，而用户完全可能在它还没跑完时就手动登录了。
    //
    // 不让位的话，下面那次 `probe()` 会 `++_generation`，把那次**已经成功**
    // 的登录判成「已被取代」，于是他看到登录成功、界面却退回登录页。
    // 手快就会中招，而且一次都复现不出来。
    var mine = _generation;
    bool superseded() => !ref.mounted || _generation != mine;
    // mock / 离线模式没有可续的会话，直接走各自的短路
    final config = ref.read(appConfigProvider);
    if (config.useMock || config.offline) {
      if (!superseded()) await probe();
      return;
    }

    final remembered = await ref.read(rememberedTokenProvider.future);
    if (superseded()) return;

    // 环境变量里那把预共享 token 不是 refresh token，续不了 —— 它走
    // 探测那条路（`/health` 说要凭据，而我们手上正好有一把）
    if (remembered != null && remembered != readSeedToken()) {
      state = state.copyWith(busy: true);
      // `restoreSession` **自己也会 `++_generation`**（它要防着自己被更新的
      // 请求盖掉）。那一次是我们发起的，不该算成「有人接手了」——
      // 算进去的话续期失败后这里直接返回，界面永远停在转圈上。
      //
      // 所以只认「除它之外还有人动过」：恰好多一次就是它自己。
      final before = _generation;
      if (await restoreSession(remembered)) return;
      if (_generation != before + 1) return;
      mine = _generation;
    }
    if (superseded()) return;
    await probe();
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
    // 离线模式：没有 cortexd 可探。直接放行 —— 停在这里探一个明知
    // 不存在的地址，只会让用户对着一个转圈的界面等 20 秒超时
    if (ref.read(appConfigProvider).offline) {
      state = state.copyWith(
        phase: AuthPhase.ready,
        health: null,
        busy: false,
        error: null,
      );
      return;
    }

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

  /// 用户名 + 密码登录。
  ///
  /// 拿回来的 refresh token 存进系统凭据库，于是**下次打开不用再登录**。
  /// access token 留在内存里 —— 它 15 分钟就过期，存下来没有意义，
  /// 而每多一处副本就多一处泄露面。
  Future<void> signInWithPassword(String username, String password) async {
    if (username.trim().isEmpty || password.isEmpty) return;
    final generation = ++_generation;
    state = state.copyWith(busy: true, error: null);

    final api = _probeApi(token: null);
    try {
      final tokens = await api.login(username.trim(), password);
      if (!_alive(generation)) return;
      // 先存再进主界面：反过来的话，存储失败会发生在用户已经"看起来登录了"
      // 之后，而他要到下次打开才发现自己没被记住
      if (kCanRememberToken) await rememberToken(tokens.refreshToken);
      if (!_alive(generation)) return;
      state = state.copyWith(
        phase: AuthPhase.ready,
        token: tokens.accessToken,
        refreshToken: tokens.refreshToken,
        busy: false,
        error: null,
        remember: true,
      );
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return;
      state = state.copyWith(busy: false, error: e.message);
    } finally {
      api.dispose();
    }
  }

  /// 拿存下来的 refresh token 换一对新的 —— **启动时走这条**。
  ///
  /// 成功就直接进主界面，用户什么都不用做；失败（过期 30 天、被登出、
  /// 或者服务端检测到重放把整条链废了）就落回登录页，并把凭据清掉 ——
  /// 留着一个已经作废的 token 只会让每次启动都白跑一次失败的请求。
  ///
  /// 返回是否续上了。
  Future<bool> restoreSession(String refreshToken) async {
    final generation = ++_generation;
    final api = _probeApi(token: null);
    try {
      final tokens = await api.refreshSession(refreshToken);
      if (!_alive(generation)) return false;
      if (kCanRememberToken) await rememberToken(tokens.refreshToken);
      state = state.copyWith(
        phase: AuthPhase.ready,
        token: tokens.accessToken,
        refreshToken: tokens.refreshToken,
        busy: false,
        error: null,
        remember: true,
      );
      return true;
    } on CortexApiException catch (e) {
      if (!_alive(generation)) return false;
      // 连不上**不算凭据失效**。清掉一个其实还有效的 token，
      // 会让「服务端暂时没起来」变成「你被登出了」
      if (!e.isUnreachable && kCanRememberToken) await forgetToken();
      return false;
    } finally {
      api.dispose();
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
    // **先告诉服务端作废，再删本地那份。**
    //
    // 只删本地只是「这台机器忘了」——已经泄露出去的那一份照样能用到 30 天后。
    // 顺序也不能反：先删本地就没有凭据去请求作废了。
    final refresh = state.refreshToken;
    if (refresh != null) {
      final api = _probeApi(token: null);
      try {
        await api.logout(refresh);
      } finally {
        api.dispose();
      }
    }
    if (kCanRememberToken) await forgetToken();
    if (!ref.mounted) return;
    state = state.copyWith(
      phase: AuthPhase.needsToken,
      token: null,
      refreshToken: null,
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

/// 上一次登录留下的 refresh token（系统凭据库 / sessionStorage）。
///
/// 做成 provider 而不是直接调 `readRememberedToken()`：那一步要跨进程问
/// 钥匙串，在测试里既跑不动也不该跑。而这条路径**曾经整个不存在**
/// （存了不读），所以它尤其需要能被测到。
final rememberedTokenProvider = FutureProvider<String?>((ref) async {
  // **必须有超时。** 读凭据库是一次跨进程调用：钥匙串锁着、libsecret 没跑、
  // 或者 D-Bus 卡住时，它可以**永远不返回** —— 而这一步挡在启动路径上，
  // 表现是应用停在一片空白上，没有任何提示，也没有任何出路。
  //
  // 超时之后按「没有记住的凭据」处理：最坏的结果是让人重登一次，
  // 而那远好过打不开。
  try {
    return await readRememberedToken().timeout(const Duration(seconds: 3));
  } on Object catch (e) {
    debugPrint('读不出记住的登录状态（$e），这次需要重新登录');
    return null;
  }
});

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
